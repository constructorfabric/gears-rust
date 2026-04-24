# Feature: Managed / Self-Managed Modes


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Conversion Request Initiation](#conversion-request-initiation)
  - [Conversion Approval](#conversion-approval)
  - [Conversion Rejection](#conversion-rejection)
  - [Conversion Cancellation](#conversion-cancellation)
  - [Parent-Side Child Conversions Discovery](#parent-side-child-conversions-discovery)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Dual-Consent Apply](#dual-consent-apply)
  - [Single-Pending Enforcement](#single-pending-enforcement)
  - [Conversion Expiry Reaper](#conversion-expiry-reaper)
  - [Creation-Time Self-Managed Admission](#creation-time-self-managed-admission)
  - [Root-Tenant Conversion Refusal](#root-tenant-conversion-refusal)
- [4. States (CDSL)](#4-states-cdsl)
  - [ConversionRequest State Machine](#conversionrequest-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Dual-Consent Approval Apply](#dual-consent-approval-apply)
  - [Single-Pending-Request Invariant](#single-pending-request-invariant)
  - [Barrier Re-Materialization Consistency](#barrier-re-materialization-consistency)
  - [Creation-Time Self-Managed Admission Bypass](#creation-time-self-managed-admission-bypass)
  - [Conversion Expiry Contract](#conversion-expiry-contract)
  - [Parent-Side Inbound-Discovery Minimal Surface](#parent-side-inbound-discovery-minimal-surface)
  - [Root-Tenant Non-Convertibility](#root-tenant-non-convertibility)
  - [Mixed-Mode Tree Consistency](#mixed-mode-tree-consistency)
  - [Dual-Consent Actor Discipline](#dual-consent-actor-discipline)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-account-management-featstatus-managed-self-managed-modes`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-account-management-feature-managed-self-managed-modes`

## 1. Feature Context

### 1.1 Overview

Owns the managed vs self-managed tenant-mode model and the durable dual-consent `ConversionRequest` workflow that moves a tenant between those modes post-creation, keeping every approval atomic with the barrier re-materialization it triggers. Mode-selection-at-creation is the non-dual-consent entry point — managed children create with no barrier, self-managed children create with the barrier materialized at activation by the hierarchy feature's closure-maintenance activation branch. Every post-creation mode flip flows through the 5-state `ConversionRequest` machine and, on approval, re-invokes the tenant-type-enforcement barrier plus the hierarchy feature's closure-maintenance algorithm in one transaction so `tenants.self_managed` and `tenant_closure.barrier` never diverge.

### 1.2 Purpose

Delivers the post-creation dual-consent conversion workflow described in PRD §5.4 and DESIGN §3.2 (ConversionService) / §3.6 (`seq-convert-dual-consent`) so that neither direction of the managed ↔ self-managed flip can happen without bilateral consent, while guaranteeing the single-pending invariant per tenant and the atomic "request status + `tenants.self_managed` + `tenant_closure.barrier`" commit set that downstream isolation depends on. Enforces that creation-time self-managed declarations skip the dual-consent flow because the parent's explicit create call is the consent, that the root tenant is never convertible, and that `cancelled`, `rejected`, and `expired` terminals leave the tenant mode unchanged. Writes the canonical `tenant_closure.barrier` by invoking `algo-closure-maintenance` from `tenant-hierarchy-management` rather than re-implementing closure writes, and re-invokes `algo-allowed-parent-types-evaluation` from `tenant-type-enforcement` at approval time so an illegal topology introduced by the mode flip is rejected before commit. Consumers: the Tenant Resolver Plugin reads the resulting `tenant_closure.barrier` on the hot path, and `tenant-metadata` uses the same `tenants.self_managed` flag as its inheritance-walk stop condition.

**Requirements**: `cpt-cf-account-management-fr-managed-tenant-creation`, `cpt-cf-account-management-fr-self-managed-tenant-creation`, `cpt-cf-account-management-fr-mode-conversion-approval`, `cpt-cf-account-management-fr-mode-conversion-expiry`, `cpt-cf-account-management-fr-mode-conversion-single-pending`, `cpt-cf-account-management-fr-mode-conversion-consistent-apply`, `cpt-cf-account-management-fr-conversion-creation-time-self-managed`, `cpt-cf-account-management-fr-child-conversions-query`, `cpt-cf-account-management-fr-conversion-cancel`, `cpt-cf-account-management-fr-conversion-reject`, `cpt-cf-account-management-fr-conversion-retention`, `cpt-cf-account-management-nfr-tenant-isolation`, `cpt-cf-account-management-nfr-barrier-enforcement`, `cpt-cf-account-management-nfr-tenant-model-versatility`

**Principles**: `cpt-cf-account-management-principle-barrier-as-data`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-account-management-actor-tenant-admin` | Initiator and counterparty for every dual-consent conversion — child-side admin acts within the child tenant scope via `/tenants/{tenant_id}/conversions`, parent-side admin acts within the parent scope via `/tenants/{tenant_id}/child-conversions`; also the actor behind managed and self-managed creation requests routed through `tenant-hierarchy-management`. |
| `cpt-cf-account-management-actor-platform-admin` | Counterparty authorized on either side per PRD §5.4 dual-consent semantics (approval / rejection of any `pending` `ConversionRequest`); never the initiator of creation-time self-managed declarations, which are always parent-tenant-admin-driven. |
| `cpt-cf-account-management-actor-tenant-resolver` | Downstream hot-path reader of the `tenant_closure.barrier` column this feature re-materializes on approval — not invoked by this feature directly, but listed to make the materialized-column consumer explicit (see `cpt-cf-account-management-principle-barrier-as-data`). |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.4 Managed/Self-Managed Tenant Modes (`fr-managed-tenant-creation`, `fr-self-managed-tenant-creation`, `fr-mode-conversion-approval`, `fr-mode-conversion-expiry`, `fr-mode-conversion-single-pending`, `fr-mode-conversion-consistent-apply`, `fr-conversion-creation-time-self-managed`, `fr-child-conversions-query`, `fr-conversion-cancel`, `fr-conversion-reject`, `fr-conversion-retention`); §6.3 Tenant Isolation Integrity (`nfr-tenant-isolation`); §6.5 Barrier Enforcement (`nfr-barrier-enforcement`); §6.6 Tenant Model Versatility (`nfr-tenant-model-versatility`).
- **Design**: [DESIGN.md](../DESIGN.md) §2.1 Barrier as Data (`principle-barrier-as-data`); §3.2 ConversionService (`component-conversion-service`); §3.6 Mode Conversion — Symmetric Dual Consent (`seq-convert-dual-consent`); §3.7 `dbtable-conversion-requests` + cross-table storage invariants; §3.8 Error Codes Reference (`pending_exists`, `invalid_actor_for_transition`, `already_resolved`, `root_tenant_cannot_convert`); ADR-0003 [`cpt-cf-account-management-adr-conversion-approval`](../ADR/0003-cpt-cf-account-management-adr-conversion-approval.md).
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.4 Managed / Self-Managed Modes.
- **Dependencies**:
  - `cpt-cf-account-management-feature-tenant-hierarchy-management` — owns `dbtable-tenants` (`self_managed` flag is toggled by this feature on approval), `dbtable-tenant-closure` (`barrier` column is rewritten here via the hierarchy feature's `algo-closure-maintenance`), and the creation saga that consumes mode selection at tenant-create time.
  - `cpt-cf-account-management-feature-tenant-type-enforcement` — owns `algo-tenant-type-enforcement-allowed-parent-types-evaluation`, re-invoked here at approval time so the post-flip topology remains legal.

## 2. Actor Flows (CDSL)

### Conversion Request Initiation

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-initiation`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Child-side initiator POSTs to `/tenants/{tenant_id}/conversions` for a non-root, `active` tenant with no existing `pending` row; ConversionService creates the `pending` `ConversionRequest` with `initiator_side=child`, `target_mode = NOT tenants.self_managed`, and `expires_at = now() + approval_ttl`; returns 201 with `request_id`.
- Parent-side initiator POSTs to `/tenants/{parent_id}/child-conversions` identifying the child tenant; ConversionService validates that the child is a direct child of the caller's parent scope and creates the `pending` row with `initiator_side=parent`.

**Error Scenarios**:

- Target tenant is the root — rejected with `root_tenant_cannot_convert` without creating any row (sub-code catalogued by `feature-errors-observability`).
- A `pending` `ConversionRequest` already exists for the target tenant — partial-unique-index collision at the DB layer, surfaced by ConversionService as `pending_exists` carrying the existing `request_id`.
- Attempt to initiate on a tenant whose `status` is not `active` (e.g., `provisioning`, `suspended`, `deleted`) — rejected with `validation` via the `errors-observability` envelope; no row created.

**Steps**:

1. [ ] - `p1` - Validate caller identity and `SecurityContext`; resolve `caller_side` from the URL collection (`/conversions` → child, `/child-conversions` → parent) via ConversionService - `inst-flow-init-validate-caller`
2. [ ] - `p1` - **IF** target tenant is the root (`parent_id IS NULL`) - `inst-flow-init-root-guard`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=root_tenant_cannot_convert)` so `errors-observability` maps to its catalogued `validation` envelope; NO row created per `algo-root-tenant-conversion-refusal` - `inst-flow-init-root-return`
3. [ ] - `p1` - **IF** target tenant is not `active` (non-convertible lifecycle state) - `inst-flow-init-status-guard`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=validation)` via the `errors-observability` envelope - `inst-flow-init-status-return`
4. [ ] - `p1` - Invoke ConversionService `initiate(caller_side, tenant_id, actor)` to attempt the `pending` insert via ConversionRepository - `inst-flow-init-service-initiate`
5. [ ] - `p1` - **IF** partial-unique-index collision on the single-pending invariant - `inst-flow-init-pending-collision`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=pending_exists)` with the existing `request_id` body per `algo-single-pending-enforcement` - `inst-flow-init-pending-return`
6. [ ] - `p1` - **RETURN** 201 Created with `{request_id, target_mode, initiator_side, status=pending, expires_at}` projected per caller scope - `inst-flow-init-success-return`

### Conversion Approval

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-approval`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Counterparty PATCHes the `pending` `ConversionRequest` with `status=approved`; ConversionService invokes `algo-dual-consent-apply` which re-evaluates the type-enforcement barrier, flips `tenants.self_managed`, re-materializes `tenant_closure.barrier` via `algo-closure-maintenance`, persists the state transition, and emits the audit entry — all in one transaction — then returns 200 with the scope-specific projection.
- Counterparty on the parent side PATCHes `/tenants/{parent_id}/child-conversions/{request_id}` when the initiator was child-side (symmetric mirror of the child-side approval path).

**Error Scenarios**:

- Caller is the initiator of this request — rejected with `invalid_actor_for_transition` carrying `attempted_status=approved` and `caller_side` (sub-code catalogued by `feature-errors-observability`).
- Request is no longer `pending` (any resolved terminal) — rejected with `already_resolved`.
- Pre-approval `algo-allowed-parent-types-evaluation` from `tenant-type-enforcement` returns reject (post-flip topology illegal) — mapped to the envelope per that feature's contract (`invalid_tenant_type` or `type_not_allowed`); no mode flip; `pending` row remains untouched for retry or explicit reject/cancel.

**Steps**:

1. [ ] - `p1` - Validate caller identity + `SecurityContext`; resolve `caller_side` from the URL collection via ConversionService - `inst-flow-appr-validate-caller`
2. [ ] - `p1` - Load the target `ConversionRequest` by `request_id` via ConversionRepository - `inst-flow-appr-load-request`
3. [ ] - `p1` - **IF** request `status ≠ pending` - `inst-flow-appr-already-resolved`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=already_resolved)` via the `errors-observability` envelope - `inst-flow-appr-already-resolved-return`
4. [ ] - `p1` - **IF** `caller_side == initiator_side` (initiator cannot approve their own request per PRD §5.4) - `inst-flow-appr-actor-guard`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=invalid_actor_for_transition, attempted_status=approved, caller_side)` - `inst-flow-appr-actor-return`
5. [ ] - `p1` - Invoke `algo-dual-consent-apply` with `(request_id, actor, caller_side)` — runs the whole approval transaction - `inst-flow-appr-apply`
6. [ ] - `p1` - **IF** dual-consent-apply returned type-enforcement reject - `inst-flow-appr-type-reject`
   1. [ ] - `p1` - **RETURN** the mapped error (`validation` / `invalid_tenant_type` OR `conflict` / `type_not_allowed`) per the envelope owned by `feature-errors-observability`; the `pending` row is left untouched for retry - `inst-flow-appr-type-reject-return`
7. [ ] - `p1` - **RETURN** 200 OK with the approved `ConversionRequest` projection (`status=approved`, `approved_by=actor`, scope-specific fields) - `inst-flow-appr-success-return`

### Conversion Rejection

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-rejection`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Counterparty PATCHes the `pending` `ConversionRequest` with `status=rejected`; ConversionService sets `status=rejected` and `rejected_by=actor` in a single transaction; `tenants.self_managed` is unchanged and no closure writes occur; audit entry emitted; returns 200 with the scope-specific projection.

**Error Scenarios**:

- Caller is the initiator (only the counterparty may reject per PRD §5.4) — rejected with `invalid_actor_for_transition` carrying `attempted_status=rejected` and `caller_side`.
- Request is no longer `pending` — rejected with `already_resolved`.

**Steps**:

1. [ ] - `p1` - Validate caller identity + `SecurityContext`; resolve `caller_side` from the URL collection via ConversionService - `inst-flow-rej-validate-caller`
2. [ ] - `p1` - Load the target `ConversionRequest` by `request_id` via ConversionRepository - `inst-flow-rej-load-request`
3. [ ] - `p1` - **IF** request `status ≠ pending` - `inst-flow-rej-already-resolved`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=already_resolved)` - `inst-flow-rej-already-resolved-return`
4. [ ] - `p1` - **IF** `caller_side == initiator_side` - `inst-flow-rej-actor-guard`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=invalid_actor_for_transition, attempted_status=rejected, caller_side)` - `inst-flow-rej-actor-return`
5. [ ] - `p1` - Invoke ConversionService `reject(caller_side, request_id, actor)` — single transaction setting `status=rejected`, `rejected_by=actor`; `tenants.self_managed` untouched; emit audit entry via `errors-observability` - `inst-flow-rej-service-reject`
6. [ ] - `p1` - **RETURN** 200 OK with the rejected `ConversionRequest` projection - `inst-flow-rej-success-return`

### Conversion Cancellation

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-cancellation`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Initiator PATCHes their own `pending` `ConversionRequest` with `status=cancelled`; ConversionService sets `status=cancelled` and `cancelled_by=actor` in a single transaction; `tenants.self_managed` is unchanged and no closure writes occur; audit entry emitted; returns 200 with the scope-specific projection.

**Error Scenarios**:

- Caller is the counterparty (only the initiator may cancel per PRD §5.4) — rejected with `invalid_actor_for_transition` carrying `attempted_status=cancelled` and `caller_side`.
- Request is no longer `pending` — rejected with `already_resolved`.

**Steps**:

1. [ ] - `p1` - Validate caller identity + `SecurityContext`; resolve `caller_side` from the URL collection via ConversionService - `inst-flow-can-validate-caller`
2. [ ] - `p1` - Load the target `ConversionRequest` by `request_id` via ConversionRepository - `inst-flow-can-load-request`
3. [ ] - `p1` - **IF** request `status ≠ pending` - `inst-flow-can-already-resolved`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=already_resolved)` - `inst-flow-can-already-resolved-return`
4. [ ] - `p1` - **IF** `caller_side ≠ initiator_side` - `inst-flow-can-actor-guard`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=invalid_actor_for_transition, attempted_status=cancelled, caller_side)` - `inst-flow-can-actor-return`
5. [ ] - `p1` - Invoke ConversionService `cancel(caller_side, request_id, actor)` — single transaction setting `status=cancelled`, `cancelled_by=actor`; `tenants.self_managed` untouched; emit audit entry via `errors-observability` - `inst-flow-can-service-cancel`
6. [ ] - `p1` - **RETURN** 200 OK with the cancelled `ConversionRequest` projection - `inst-flow-can-success-return`

### Parent-Side Child Conversions Discovery

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-managed-self-managed-modes-parent-child-conversions-discovery`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Parent-tenant admin GETs `/tenants/{parent_id}/child-conversions` with optional `status_filter` and pagination params; ConversionService returns the paginated list of `ConversionRequest` rows for direct children of `{parent_id}`, projecting only the minimal cross-barrier surface per DESIGN §3.2 — `request_id`, child `tenant_id`, `child_name`, `initiator_side`, `target_mode`, `status`, actor uuids, and timestamps — without exposing child subtree metadata.
- Parent-tenant admin GETs `/tenants/{parent_id}/child-conversions/{request_id}` for a specific request, receiving the same minimal projection for a single row.

**Error Scenarios**:

- `{parent_id}` does not resolve to a tenant the caller is authorized to view — rejected with `validation` via the envelope (`cross_tenant_denied` may apply per PolicyEnforcer decision, which is outside this feature's scope).
- `{request_id}` does not belong to a direct child of `{parent_id}` — rejected with `not_found` via the envelope (catalogued by `feature-errors-observability`).

**Steps**:

1. [ ] - `p1` - Validate caller identity + `SecurityContext`; confirm collection URL is parent-scope (`/child-conversions`) via ConversionService - `inst-flow-pdis-validate-caller`
2. [ ] - `p1` - **IF** request is the list endpoint - `inst-flow-pdis-list-branch`
   1. [ ] - `p1` - Invoke ConversionService `list_inbound_for_parent(parent_id, status_filter, pagination)` via ConversionRepository; scope to direct children of `{parent_id}` only - `inst-flow-pdis-list-service`
   2. [ ] - `p1` - Project the minimal cross-barrier fields (no child subtree, no child tenant record beyond `child_name`) per DESIGN §3.2 - `inst-flow-pdis-list-project`
   3. [ ] - `p1` - **RETURN** 200 OK with the paginated minimal projection - `inst-flow-pdis-list-return`
3. [ ] - `p1` - **ELSE** request is the single-item endpoint - `inst-flow-pdis-item-branch`
   1. [ ] - `p1` - Load `ConversionRequest` by `request_id` via ConversionRepository; confirm the target tenant's `parent_id` equals `{parent_id}` - `inst-flow-pdis-item-load`
   2. [ ] - `p1` - **IF** not found OR not a direct child of `{parent_id}` - `inst-flow-pdis-item-not-found`
      1. [ ] - `p1` - **RETURN** `(reject, sub_code=not_found)` via the `errors-observability` envelope - `inst-flow-pdis-item-not-found-return`
   3. [ ] - `p1` - **RETURN** 200 OK with the minimal projection for the single row - `inst-flow-pdis-item-return`

## 3. Processes / Business Logic (CDSL)

### Dual-Consent Apply

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply`

**Input**: `request_id`, approving `actor`, `caller_side` (child / parent).

**Output**: Committed approval in ONE transaction — `ConversionRequest.status=approved`, `tenants.self_managed` flipped, `tenant_closure.barrier` re-materialized, audit entry emitted — OR a mapped pre-approval reject from the type-enforcement barrier with no state change.

**Steps**:

> Single-transaction boundary is mandatory per `fr-mode-conversion-consistent-apply` and DESIGN §3.6 `seq-convert-dual-consent`. Closure writes are delegated to `algo-closure-maintenance` from `tenant-hierarchy-management`; type re-evaluation is delegated to `algo-allowed-parent-types-evaluation` from `tenant-type-enforcement`; audit emission uses the envelope from `feature-errors-observability`.

1. [ ] - `p1` - Begin transaction on the owning `tenants` + `conversion_requests` + `tenant_closure` write surface via ConversionService - `inst-algo-dca-begin-tx`
2. [ ] - `p1` - Load the `pending` `ConversionRequest` by `request_id` via ConversionRepository under the TX; re-confirm `status=pending` and `caller_side ≠ initiator_side` to guard against concurrent transitions - `inst-algo-dca-reload-request`
3. [ ] - `p1` - Load the target tenant row via TenantRepository under the TX; read current `tenant_type` and `parent_tenant_type` for the post-flip topology check - `inst-algo-dca-load-tenant`
4. [ ] - `p1` - Invoke `algo-allowed-parent-types-evaluation` from `tenant-type-enforcement` with the current `(child_tenant_type, parent_tenant_type)` as the pre-approval guard - `inst-algo-dca-type-check`
5. [ ] - `p1` - **IF** type evaluation returned reject - `inst-algo-dca-type-reject`
   1. [ ] - `p1` - Rollback the transaction; `pending` row remains untouched for retry or explicit reject/cancel - `inst-algo-dca-type-rollback`
   2. [ ] - `p1` - **RETURN** the mapped reject (e.g., `invalid_tenant_type` or `type_not_allowed`) for the flow to project via the `errors-observability` envelope - `inst-algo-dca-type-return`
6. [ ] - `p1` - Flip `tenants.self_managed` to `target_mode` on the target tenant via TenantRepository under the TX - `inst-algo-dca-flip-self-managed`
7. [ ] - `p1` - Invoke `algo-closure-maintenance` from `tenant-hierarchy-management` on the barrier-re-materialization branch for every affected `(ancestor, descendant]` path touching the converted tenant; barrier value derived from the canonical rule per `principle-barrier-as-data` - `inst-algo-dca-barrier-rematerialize`
8. [ ] - `p1` - Transition the `ConversionRequest` row to `status=approved`, `approved_by=actor` via ConversionRepository under the TX - `inst-algo-dca-transition-approved`
9. [ ] - `p1` - Emit the audit entry via the `errors-observability` audit envelope, referencing the `conversion` audit event kind (authoritative kind catalogued in `feature-errors-observability`, e.g., `conversion_approved`) - `inst-algo-dca-audit-emit`
10. [ ] - `p1` - Commit the transaction so request status, `tenants.self_managed`, `tenant_closure.barrier`, and the audit record all become visible atomically per `fr-mode-conversion-consistent-apply` - `inst-algo-dca-commit`
11. [ ] - `p1` - **RETURN** success with the approved projection to the calling flow - `inst-algo-dca-return-success`

### Single-Pending Enforcement

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-managed-self-managed-modes-single-pending-enforcement`

**Input**: `tenant_id`, candidate `ConversionRequest` initiation context.

**Output**: Either a new `pending` row persisted, or a `pending_exists` conflict carrying the existing `request_id`; no partial state either way.

**Steps**:

> The at-most-one-pending-per-tenant invariant is enforced at the storage layer by the partial unique index catalogued in DESIGN §3.7 `dbtable-conversion-requests`. This algorithm is the service-layer contract that translates the DB collision into the public sub-code.

1. [ ] - `p1` - Attempt the `pending` `ConversionRequest` insert via ConversionRepository with `initiator_side`, `target_mode`, `expires_at = now() + approval_ttl` - `inst-algo-spe-attempt-insert`
2. [ ] - `p1` - **IF** insert raised the partial-unique-index collision on the single-pending invariant - `inst-algo-spe-collision-detected`
   1. [ ] - `p1` - Load the existing `pending` row for `{tenant_id}` via ConversionRepository to retrieve its `request_id` - `inst-algo-spe-load-existing`
   2. [ ] - `p1` - **RETURN** `(reject, sub_code=pending_exists, existing_request_id)` — surfaced by the service layer as a `conflict` envelope entry per `feature-errors-observability` - `inst-algo-spe-return-conflict`
3. [ ] - `p1` - **ELSE** insert succeeded - `inst-algo-spe-success`
   1. [ ] - `p1` - **RETURN** success with the new `request_id` - `inst-algo-spe-return-success`

### Conversion Expiry Reaper

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-managed-self-managed-modes-conversion-expiry-reaper`

**Input**: Scheduled invocation every `cleanup_interval`, with the `expires_at` cutoff of `now()`.

**Output**: `pending` `ConversionRequest` rows whose `expires_at` has elapsed are transitioned to `expired`; tenant mode unchanged; audit entries emitted per row; `am_conversion_expired_total` metric advanced by the reaped count.

**Steps**:

> Background reaper is the only actor authorized to drive a `pending` row to `expired` per PRD §5.4 and DESIGN §3.2 ConversionService `expire` operation. Tenant mode MUST NOT change on expiry per `fr-mode-conversion-expiry`.

1. [ ] - `p1` - Query ConversionRepository for `pending` rows whose `expires_at` has elapsed (cutoff = `now()`) with a bounded batch size - `inst-algo-cer-query-expired`
2. [ ] - `p1` - **IF** no rows match - `inst-algo-cer-empty-batch`
   1. [ ] - `p1` - **RETURN** no-op; next tick will re-query - `inst-algo-cer-empty-return`
3. [ ] - `p1` - **FOR EACH** matched row - `inst-algo-cer-each-row`
   1. [ ] - `p1` - Begin transaction via ConversionService - `inst-algo-cer-begin-tx`
   2. [ ] - `p1` - Transition row to `status=expired` via ConversionRepository; `tenants.self_managed` untouched per `fr-mode-conversion-expiry` - `inst-algo-cer-transition`
   3. [ ] - `p1` - Emit audit entry with `actor=system` via the `errors-observability` audit envelope per `nfr-audit-completeness` - `inst-algo-cer-audit-emit`
   4. [ ] - `p1` - Commit - `inst-algo-cer-commit`
4. [ ] - `p1` - Advance `am_conversion_expired_total` metric by the reaped count via `errors-observability` metric envelope - `inst-algo-cer-metric`
5. [ ] - `p1` - **RETURN** reaped count to the scheduler - `inst-algo-cer-return`

### Creation-Time Self-Managed Admission

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-managed-self-managed-modes-creation-time-self-managed-admission`

**Input**: Validated child-creation request carrying `self_managed` (boolean) from the `tenant-hierarchy-management` create saga.

**Output**: Admit / bypass decision — `true` means the saga proceeds without any `ConversionRequest`, and the barrier (when `self_managed=true`) is materialized by `algo-closure-maintenance` activation branch at saga step 3 per `principle-barrier-as-data`.

**Steps**:

> Per `fr-conversion-creation-time-self-managed`, the parent's explicit create call IS the dual-consent signal at creation time — no `ConversionRequest` row is written. Barrier materialization at activation is owned by `tenant-hierarchy-management`; this feature only asserts the bypass decision. Root-tenant creation does not flow through this algorithm — `feature-platform-bootstrap` inserts the root row directly via `TenantService` with `self_managed=false` by deployment convention, bypassing the hierarchy-management create saga that invokes this admission check.

1. [ ] - `p1` - **IF** request `self_managed == true` - `inst-algo-ctsma-selfmanaged-branch`
   1. [ ] - `p1` - Mark the create path as "bypass dual-consent" and record that the create saga itself counts as consent per `fr-conversion-creation-time-self-managed` - `inst-algo-ctsma-bypass-mark`
   2. [ ] - `p1` - **RETURN** admit; saga step 3 will materialize the barrier via `algo-closure-maintenance` activation branch (ancestor-walk carries `barrier=1` for the strict `(ancestor, self]` entries because `tenants.self_managed=true` is already persisted by saga step 1) - `inst-algo-ctsma-selfmanaged-return`
2. [ ] - `p1` - **ELSE** request `self_managed == false` (managed creation) - `inst-algo-ctsma-managed-branch`
   1. [ ] - `p1` - **RETURN** admit; saga step 3 materializes closure rows with `barrier=0` on every self-row and with strict-ancestor barriers derived from any self-managed ancestor that already exists per the canonical rule - `inst-algo-ctsma-managed-return`

### Root-Tenant Conversion Refusal

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-managed-self-managed-modes-root-tenant-conversion-refusal`

**Input**: `tenant_id` presented by an initiation attempt.

**Output**: Reject with `root_tenant_cannot_convert` and no `ConversionRequest` row created, OR pass-through when `tenant_id` is a non-root tenant (non-root refusal is the responsibility of the initiation flow's other guards).

**Steps**:

> Per PRD §5.4 and DESIGN §3.8, the root tenant is never convertible. This algorithm is the authoritative refusal point; `flow-conversion-initiation` cites it via `inst-flow-init-root-guard`.

1. [ ] - `p1` - Load the target tenant row via TenantRepository - `inst-algo-rtcr-load-tenant`
2. [ ] - `p1` - **IF** tenant is the root (`parent_id IS NULL`) - `inst-algo-rtcr-root-check`
   1. [ ] - `p1` - **RETURN** `(reject, sub_code=root_tenant_cannot_convert)` — catalogued authoritatively by `feature-errors-observability`; NO `ConversionRequest` row is created - `inst-algo-rtcr-root-return`
3. [ ] - `p1` - **ELSE** tenant is non-root - `inst-algo-rtcr-nonroot-branch`
   1. [ ] - `p1` - **RETURN** pass-through so the initiation flow can apply its remaining guards (status, single-pending, etc.) - `inst-algo-rtcr-nonroot-return`

## 4. States (CDSL)

### ConversionRequest State Machine

- [ ] `p1` - **ID**: `cpt-cf-account-management-state-managed-self-managed-modes-conversion-request`

**States**: `pending`, `approved`, `cancelled`, `rejected`, `expired`

**Initial State**: `pending`

**State Semantics**:

- `pending` — `dbtable-conversion-requests.state = 'pending'`; the only state in which `tenants.self_managed` may later flip; exactly one `pending` row per `tenant_id` is admissible per the partial-unique-index invariant owned by `algo-single-pending-enforcement`; `expires_at` is populated at insert time and monitored by `algo-conversion-expiry-reaper`.
- `approved` — `dbtable-conversion-requests.state = 'approved'`; terminal; `tenants.self_managed` has already been flipped to `target_mode` and `tenant_closure.barrier` has been re-materialized via `algo-closure-maintenance`, all inside the single transaction owned by `algo-dual-consent-apply`; `approved_by = counterparty actor uuid`.
- `cancelled` — `dbtable-conversion-requests.state = 'cancelled'`; terminal; initiator withdrew the request before any counterparty decision; `tenants.self_managed` is unchanged and no closure writes occurred; `cancelled_by = initiator actor uuid`.
- `rejected` — `dbtable-conversion-requests.state = 'rejected'`; terminal; counterparty refused the request; `tenants.self_managed` is unchanged and no closure writes occurred; `rejected_by = counterparty actor uuid`.
- `expired` — `dbtable-conversion-requests.state = 'expired'`; terminal; background reaper observed `now() >= expires_at`; `tenants.self_managed` is unchanged and `actor=system` per the audit envelope; metric `am_conversion_expired_total` is advanced by the reaped count.

**Transitions**:

1. [ ] - `p1` - **FROM** `pending` **TO** `approved` **WHEN** `algo-dual-consent-apply` commits its single transaction (pre-approval barrier guard via `algo-allowed-parent-types-evaluation` passed; `tenants.self_managed` flipped; `tenant_closure.barrier` re-materialized via `algo-closure-maintenance`; request row transitioned; audit entry emitted) - `inst-state-conversion-pending-to-approved`
2. [ ] - `p1` - **FROM** `pending` **TO** `cancelled` **WHEN** the initiator PATCHes their own row on the initiator-side collection (`caller_side == initiator_side`) per `flow-conversion-cancellation` - `inst-state-conversion-pending-to-cancelled`
3. [ ] - `p1` - **FROM** `pending` **TO** `rejected` **WHEN** the counterparty PATCHes the row on the counterparty-side collection (`caller_side != initiator_side`) per `flow-conversion-rejection` - `inst-state-conversion-pending-to-rejected`
4. [ ] - `p1` - **FROM** `pending` **TO** `expired` **WHEN** `algo-conversion-expiry-reaper` observes `now() >= expires_at` on a `pending` row; `tenants.self_managed` MUST NOT change on this transition per `fr-mode-conversion-expiry` - `inst-state-conversion-pending-to-expired`

## 5. Definitions of Done

### Dual-Consent Approval Apply

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-apply`

The system **MUST** execute every dual-consent approval as a single transaction that commits, in this order, the pre-approval barrier guard via `algo-allowed-parent-types-evaluation`, the `tenants.self_managed` flip to `target_mode`, the `tenant_closure.barrier` re-materialization delegated to `algo-closure-maintenance` on every affected `(ancestor, descendant]` path, the `ConversionRequest.state` transition to `approved` with `approved_by = counterparty actor uuid`, and the `conversion_approved` audit entry emitted through the `feature-errors-observability` audit envelope. The approval **MUST** be driven by the counterparty only; the initiator **MUST NOT** be able to approve their own request. If the pre-approval barrier guard rejects, the transaction **MUST** roll back and the `pending` row **MUST** remain untouched for retry or explicit reject/cancel. Partial commits (for example, `tenants.self_managed` flipped without `tenant_closure.barrier` re-materialized, or request status transitioned without audit emission) **MUST NOT** be externally observable under any failure mode.

**Implements**:

- `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-approval`
- `cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply`

**Constraints**: `cpt-cf-account-management-principle-barrier-as-data`, `cpt-cf-account-management-adr-conversion-approval`

**Touches**:

- Entities: `ConversionRequest`, `TenantMode`
- Data: `cpt-cf-account-management-dbtable-conversion-requests`, `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure`
- Sibling integration: `cpt-cf-account-management-algo-tenant-type-enforcement-allowed-parent-types-evaluation`, `cpt-cf-account-management-algo-tenant-hierarchy-management-closure-maintenance`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); sub-codes referenced by name only (`invalid_tenant_type`, `type_not_allowed`, `invalid_actor_for_transition`, `already_resolved`), HTTP status mapping owned by `feature-errors-observability`.

### Single-Pending-Request Invariant

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-managed-self-managed-modes-single-pending-invariant`

The system **MUST** enforce the at-most-one-`pending`-`ConversionRequest`-per-tenant invariant at the storage layer via the partial unique index declared by `dbtable-conversion-requests`. ConversionService **MUST** translate the DB-layer constraint-violation into the public `pending_exists` sub-code carrying the existing `request_id`, surfaced through the `feature-errors-observability` envelope. The service **MUST NOT** substitute a read-then-insert pattern that would open a lost-update window; the storage constraint is the authoritative guard. Subsequent initiation attempts on a tenant that already has a resolved (`approved` / `cancelled` / `rejected` / `expired`) row **MUST** succeed because only `pending` rows participate in the partial-unique index.

**Implements**:

- `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-initiation`
- `cpt-cf-account-management-algo-managed-self-managed-modes-single-pending-enforcement`

**Touches**:

- Entities: `ConversionRequest`
- Data: `cpt-cf-account-management-dbtable-conversion-requests`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `pending_exists` sub-code referenced by name only, HTTP status mapping owned by `feature-errors-observability`.

### Barrier Re-Materialization Consistency

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-managed-self-managed-modes-barrier-rematerialization-consistency`

The system **MUST** ensure that after every approved conversion, every `tenant_closure` row `(ancestor, descendant)` whose strict path `(ancestor, descendant]` touches the converted tenant has its `barrier` column recomputed from the canonical invariant: `barrier = 1` iff any tenant on the strict path is `self_managed = true`. Zero half-updated `tenant_closure` rows **MUST** be visible outside the owning approval transaction. The feature **MUST NOT** re-implement closure writes; barrier re-materialization is delegated to `algo-closure-maintenance` owned by `feature-tenant-hierarchy-management`, invoked inside the approval transaction so that request state, `tenants.self_managed`, and `tenant_closure.barrier` all become visible atomically.

**Implements**:

- `cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply`
- `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-approval`

**Constraints**: `cpt-cf-account-management-principle-barrier-as-data`

**Touches**:

- Entities: `TenantMode`
- Data: `cpt-cf-account-management-dbtable-tenant-closure`
- Sibling integration: `cpt-cf-account-management-algo-tenant-hierarchy-management-closure-maintenance`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner).

### Creation-Time Self-Managed Admission Bypass

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-managed-self-managed-modes-creation-time-self-managed`

The system **MUST** bypass the dual-consent flow when `self_managed = true` is set at tenant-creation time because the parent's explicit create call is the consent signal per `fr-conversion-creation-time-self-managed`. No `ConversionRequest` row **MUST** be inserted on the creation path, and the conversion-initiation REST endpoints **MUST NOT** be invoked as part of creation. The barrier **MUST** still be materialized at activation as part of the `algo-closure-maintenance` activation branch so that strict `(ancestor, self]` closure entries carry `barrier = 1`. Root-tenant creation **MUST** force `self_managed = false` because the root is non-convertible by deployment convention.

**Implements**:

- `cpt-cf-account-management-algo-managed-self-managed-modes-creation-time-self-managed-admission`

**Touches**:

- Entities: `TenantMode`
- Data: `cpt-cf-account-management-dbtable-tenants`, `cpt-cf-account-management-dbtable-tenant-closure`
- Sibling integration: `cpt-cf-account-management-algo-tenant-hierarchy-management-closure-maintenance`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner).

### Conversion Expiry Contract

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-managed-self-managed-modes-conversion-expiry`

The system **MUST** populate `expires_at` on every `pending` `ConversionRequest` insert. A background reaper **MUST** transition rows whose `expires_at` is in the past to `expired` without mutating `tenants.self_managed` or `tenant_closure.barrier`, and **MUST** emit an audit entry with `actor = system` through the `feature-errors-observability` audit envelope and advance the `am_conversion_expired_total` metric by the reaped count. Resolved rows (`approved` / `cancelled` / `rejected` / `expired`) **MUST** remain queryable on the default API surface until the configured retention window elapses, after which the soft-delete-and-hard-delete retention cadence owned by `feature-tenant-hierarchy-management` reclaims them via `ON DELETE CASCADE` on `conversion_requests.tenant_id`. The resolved-retention window **MUST NOT** exceed the tenant hard-delete retention period.

**Implements**:

- `cpt-cf-account-management-algo-managed-self-managed-modes-conversion-expiry-reaper`
- `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-initiation`

**Touches**:

- Entities: `ConversionRequest`
- Data: `cpt-cf-account-management-dbtable-conversion-requests`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); audit-kind `conversion_expired` and metric `am_conversion_expired_total` catalogued by `feature-errors-observability`.

### Parent-Side Inbound-Discovery Minimal Surface

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-managed-self-managed-modes-parent-side-minimal-surface`

The system **MUST** expose only minimal conversion-request metadata across the self-managed barrier on `GET /tenants/{parent_id}/child-conversions` and `GET /tenants/{parent_id}/child-conversions/{request_id}`: `request_id`, `tenant_id`, `child_name`, `initiator_side`, `target_mode`, `status`, actor uuids (`requested_by`, `approved_by`, `cancelled_by`, `rejected_by`), and timestamps per DESIGN §3.2 `list_inbound_for_parent`. Child-subtree data (tenant metadata, descendants, user records, resource inventories) **MUST NOT** leak through this endpoint. Cross-barrier reads **MUST NOT** bypass the barrier invariant for any field beyond this minimal projection; AuthZ is delegated to `PolicyEnforcer::enforce` on `ConversionRequest.read` at the REST layer before the service call is made.

**Implements**:

- `cpt-cf-account-management-flow-managed-self-managed-modes-parent-child-conversions-discovery`

**Touches**:

- Entities: `ConversionRequest`
- Data: `cpt-cf-account-management-dbtable-conversion-requests`
- Sibling integration: `PolicyEnforcer` at the REST layer (external to this feature's surface)
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `not_found` and `validation` sub-codes referenced by name only.

### Root-Tenant Non-Convertibility

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-managed-self-managed-modes-root-tenant-non-convertibility`

The system **MUST** refuse every initiation attempt targeting the root tenant (`parent_id IS NULL`) with `sub_code = root_tenant_cannot_convert`, surfaced through the `feature-errors-observability` envelope. No `ConversionRequest` row **MUST** be created for the root tenant under any code path, including creation-time admission (the root is always admitted with `self_managed = false`). The refusal **MUST** occur at the initiation boundary before any other guard runs; `flow-conversion-initiation` invokes `algo-root-tenant-conversion-refusal` as the first post-authorization check.

**Implements**:

- `cpt-cf-account-management-algo-managed-self-managed-modes-root-tenant-conversion-refusal`
- `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-initiation`

**Touches**:

- Entities: `ConversionRequest`
- Data: `cpt-cf-account-management-dbtable-tenants`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `root_tenant_cannot_convert` sub-code referenced by name only.

### Mixed-Mode Tree Consistency

- [ ] `p2` - **ID**: `cpt-cf-account-management-dod-managed-self-managed-modes-mixed-mode-tree-consistency`

The system **MUST** support managed and self-managed tenants coexisting in the same hierarchy without rejecting tree shapes that mix modes across ancestor chains. This feature **MUST** write the `tenant_closure.barrier` column via `algo-closure-maintenance` on every conversion approval and on every creation-time admission, so that the materialized barrier accurately reflects whether any tenant on each strict `(ancestor, descendant]` path is `self_managed = true`. Hot-path read semantics over the materialized barrier are owned by `tenant-resolver-plugin` and **MUST NOT** be re-implemented here; this feature's responsibility ends at writing the canonical `barrier` value.

**Implements**:

- `cpt-cf-account-management-algo-managed-self-managed-modes-dual-consent-apply`
- `cpt-cf-account-management-algo-managed-self-managed-modes-creation-time-self-managed-admission`

**Constraints**: `cpt-cf-account-management-principle-barrier-as-data`

**Touches**:

- Entities: `TenantMode`, `BarrierMode`
- Data: `cpt-cf-account-management-dbtable-tenant-closure`
- Sibling integration: `tenant-resolver-plugin` (downstream hot-path reader; out-of-feature)
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner).

### Dual-Consent Actor Discipline

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-managed-self-managed-modes-dual-consent-actor-discipline`

The system **MUST** enforce that only the counterparty of a `pending` `ConversionRequest` can approve or reject it, and only the initiator can cancel their own request. Any other `(caller_side, transition)` combination **MUST** be refused with `sub_code = invalid_actor_for_transition`, carrying `attempted_status` and `caller_side` for observability, through the `feature-errors-observability` envelope. Idempotent PATCH attempts on `approved`, `cancelled`, `rejected`, or `expired` rows **MUST** be refused with `sub_code = already_resolved`; the resolved state **MUST NOT** be re-written under any condition. Role evaluation **MUST** happen after the `pending`-state check so that a resolved row returns `already_resolved` regardless of who the caller is.

**Implements**:

- `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-approval`
- `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-rejection`
- `cpt-cf-account-management-flow-managed-self-managed-modes-conversion-cancellation`

**Touches**:

- Entities: `ConversionRequest`
- Data: `cpt-cf-account-management-dbtable-conversion-requests`
- Error taxonomy: delegated to `feature-errors-observability` (catalog owner); `invalid_actor_for_transition` and `already_resolved` sub-codes referenced by name only, HTTP status mapping owned by `feature-errors-observability`.

## 6. Acceptance Criteria

- [ ] A counterparty PATCH approving a `pending` `ConversionRequest` flips `tenants.self_managed` to `target_mode`, re-materializes `tenant_closure.barrier` via `algo-closure-maintenance` on every `(ancestor, descendant]` strict path touching the converted tenant, transitions `dbtable-conversion-requests.state` to `approved` with `approved_by = counterparty actor uuid`, and emits the `conversion_approved` audit entry — all visible atomically inside one transaction; failure of any sub-step rolls back the whole transaction and leaves no half-applied state externally observable. Fingerprints `dod-managed-self-managed-modes-dual-consent-apply`.
- [ ] Attempting to create a second `pending` `ConversionRequest` for a tenant that already has a `pending` row returns `sub_code=pending_exists` carrying the existing `request_id`, surfaced through the `feature-errors-observability` envelope; the partial unique index declared on `dbtable-conversion-requests` raises the storage-layer collision so the service-layer translation cannot be bypassed by a read-then-insert race. A subsequent initiation attempt on the same tenant after the prior row is resolved (`approved` / `cancelled` / `rejected` / `expired`) succeeds because only `pending` rows participate in the partial unique index. Fingerprints `dod-managed-self-managed-modes-single-pending-invariant`.
- [ ] A PATCH approval or rejection invoked by the initiator on their own `pending` row (`caller_side == initiator_side`) returns `sub_code=invalid_actor_for_transition` carrying `attempted_status` and `caller_side`; `dbtable-conversion-requests.state` is not mutated and `tenants.self_managed` is unchanged. Fingerprints `dod-managed-self-managed-modes-dual-consent-actor-discipline`.
- [ ] A PATCH cancellation invoked by the counterparty (`caller_side ≠ initiator_side`) on a `pending` row returns `sub_code=invalid_actor_for_transition` carrying `attempted_status=cancelled` and `caller_side`; only the initiator can cancel their own `pending` row, and the successful initiator-side cancel sets `dbtable-conversion-requests.state=cancelled` with `cancelled_by = initiator actor uuid` while leaving `tenants.self_managed` unchanged. Fingerprints `dod-managed-self-managed-modes-dual-consent-actor-discipline`.
- [ ] A PATCH attempt (approve, reject, or cancel) on a `ConversionRequest` row whose state is already `approved`, `cancelled`, `rejected`, or `expired` returns `sub_code=already_resolved` regardless of the caller's side; the resolved state is not re-written, and the role check does not run for resolved rows because the state check precedes actor discipline. Fingerprints `dod-managed-self-managed-modes-dual-consent-actor-discipline`.
- [ ] Approving a `managed → self-managed` conversion causes every `tenant_closure` row whose strict `(ancestor, descendant]` path includes the newly self-managed tenant to have `barrier = 1`; approving a `self-managed → managed` conversion causes every `tenant_closure` row whose strict `(ancestor, descendant]` path no longer contains any `self_managed = true` tenant to have `barrier = 0`, while rows whose strict path is unaffected are not rewritten. Zero half-updated `tenant_closure` rows are visible outside the owning approval transaction because closure re-materialization is delegated to `algo-closure-maintenance` inside the same transaction that flips `tenants.self_managed`. Fingerprints `dod-managed-self-managed-modes-barrier-rematerialization-consistency`.
- [ ] The background reaper driven by `algo-conversion-expiry-reaper` transitions every `pending` `ConversionRequest` for which `now() >= expires_at` (equivalently `expires_at <= now()`) to `dbtable-conversion-requests.state=expired` without mutating `tenants.self_managed` or `tenant_closure.barrier`; for each reaped row an audit entry with `actor = system` is emitted via the `feature-errors-observability` audit envelope, and `am_conversion_expired_total` is advanced by the reaped count. Fingerprints `dod-managed-self-managed-modes-conversion-expiry`.
- [ ] A `ConversionRequest` row in a resolved state (`approved`, `cancelled`, `rejected`, or `expired`) remains queryable on the default `/tenants/{tenant_id}/conversions` and `/tenants/{tenant_id}/child-conversions` API surfaces until the soft-delete-and-hard-delete retention cadence owned by `feature-tenant-hierarchy-management` reclaims it via `ON DELETE CASCADE` on `conversion_requests.tenant_id`; no `ConversionRequest` row is deleted solely because it transitioned out of `pending`, and the resolved-retention window does not exceed the tenant hard-delete retention period. Fingerprints `dod-managed-self-managed-modes-conversion-expiry`.
- [ ] Creating a child tenant with `self_managed = true` at tenant-creation time via the `tenant-hierarchy-management` create saga writes no `ConversionRequest` row and does not invoke the `/tenants/{tenant_id}/conversions` REST surface; `tenant_closure.barrier = 1` is materialized at activation by the `algo-closure-maintenance` activation branch for every `(ancestor, descendant]` strict path whose descendant is the new self-managed tenant. A root-creation path (`parent_id IS NULL`) forces `self_managed = false` regardless of the caller's requested value because the root is non-convertible by deployment convention. Fingerprints `dod-managed-self-managed-modes-creation-time-self-managed`.
- [ ] A `GET /tenants/{parent_id}/child-conversions` response includes for each conversion-request entry only the minimal cross-barrier projection per DESIGN §3.2 — `request_id`, child `tenant_id`, `child_name`, `initiator_side`, `target_mode`, `status`, actor uuids (`requested_by`, `approved_by`, `cancelled_by`, `rejected_by`), and timestamps; no child-subtree data (tenant metadata beyond `child_name`, descendants, user records, or resource inventories) is surfaced. A `GET /tenants/{parent_id}/child-conversions/{request_id}` targeting a `request_id` that does not belong to a direct child of `{parent_id}` returns `sub_code=not_found` via the `feature-errors-observability` envelope. Fingerprints `dod-managed-self-managed-modes-parent-side-minimal-surface`.
- [ ] A `POST /tenants/{root_id}/conversions` targeting the root tenant (`parent_id IS NULL`) is refused with `sub_code=root_tenant_cannot_convert` through the `feature-errors-observability` envelope; no row is inserted into `dbtable-conversion-requests`. The refusal applies regardless of which actor initiates the request and is driven by `algo-root-tenant-conversion-refusal` as the first post-authorization guard of `flow-conversion-initiation`, before single-pending or status guards run. Fingerprints `dod-managed-self-managed-modes-root-tenant-non-convertibility`.
- [ ] A hierarchy containing managed and self-managed tenants mixed across ancestor chains is admitted by both approval and creation paths without tree-shape rejections; on every conversion approval and every creation-time admission, `tenant_closure.barrier` is written via `algo-closure-maintenance` so that every `(ancestor, descendant]` strict path reflects the canonical invariant (`barrier = 1` iff any tenant on the strict path is `self_managed = true`). Hot-path read semantics over the materialized column are not implemented here — this AC binds the write-side contract only. Fingerprints `dod-managed-self-managed-modes-mixed-mode-tree-consistency`.

## 7. Deliberate Omissions

- **Tenant-type compatibility matrix, GTS types registry integration, and same-type nesting evaluation** — *Owned by `cpt-cf-account-management-feature-tenant-type-enforcement`* (DECOMPOSITION §2.3). This feature only invokes `algo-tenant-type-enforcement-allowed-parent-types-evaluation` at approval time as the pre-approval guard inside `algo-dual-consent-apply`; the registry lookup, `allowed_parent_types` evaluation rules, and same-type nesting logic live there.
- **Tenant creation, update, soft-delete, hard-delete, closure-table schema, and closure maintenance algorithm authoring** — *Owned by `cpt-cf-account-management-feature-tenant-hierarchy-management`* (DECOMPOSITION §2.2). This feature triggers closure re-materialization via `algo-closure-maintenance` at approval time and at creation-time activation but does not author closure maintenance, status transitions, or retention cadence.
- **Cross-cutting error taxonomy, RFC 9457 envelope, audit pipeline, reliability / SLA policy, and metric-catalog naming-alignment contract** — *Owned by `cpt-cf-account-management-feature-errors-observability`* (DECOMPOSITION §2.8). Sub-codes `pending_exists`, `invalid_actor_for_transition`, `already_resolved`, `root_tenant_cannot_convert`, `not_found`, and `validation` are catalogued authoritatively there; this feature emits them by name and defers envelope formatting, HTTP status mapping, audit emission (`conversion_approved`, `conversion_rejected`, `conversion_cancelled`, `conversion_expired`), and metric sample naming (`am_conversion_expired_total`) to that feature.
- **Identity, authentication, session, token issuance, and user / credential lifecycle** — *Inherited from the platform AuthN layer and the IdP contract* (DESIGN §4.2). AM trusts the normalized `SecurityContext` on every operation surfaced by this feature and does not validate tokens, federate identities, or issue sessions; `SecurityContext` validation is a platform-layer precondition.
- **AuthZ read-path policy evaluation, barrier enforcement on reads, and `BarrierMode` reductions applied to queries** — *Owned by `PolicyEnforcer` / AuthZ Resolver / `tenant-resolver-plugin`* (AM DECOMPOSITION §2.9, authoritatively defined in the `cf-tr-plugin` sub-system DECOMPOSITION). This feature is the source-of-truth writer of the `tenant_closure.barrier` column; the Tenant Resolver Plugin is the query-time reader and applies `BarrierMode` to downstream reads for billing / administrative bypass scenarios.
- **Root-tenant bootstrap creation and platform bring-up** — *Owned by `cpt-cf-account-management-feature-platform-bootstrap`* (DECOMPOSITION §2.1). This feature explicitly refuses mode-conversion requests for the root tenant and forces `self_managed = false` at root-creation admission, but does not own root-creation itself.
- **`BarrierMode` hot-path query semantics and selective barrier-bypass routing for billing and administrative reads** — *Owned by `tenant-resolver-plugin`* (DECOMPOSITION §2.9). This feature writes the `barrier` column in `dbtable-tenant-closure` and declares the source-of-truth invariant; the plugin is the hot-path reader that applies `BarrierMode` reductions when resolving query-time tenant scope.
