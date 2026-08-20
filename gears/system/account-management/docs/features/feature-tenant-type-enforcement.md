# Feature: Tenant Type Enforcement


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Tenant Type Check on Child Create](#tenant-type-check-on-child-create)
  - [Tenant Type Re-evaluation on Mode Conversion](#tenant-type-re-evaluation-on-mode-conversion)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Allowed Parent Types Evaluation](#allowed-parent-types-evaluation)
  - [Same-Type Nesting Admission](#same-type-nesting-admission)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Barrier Invocation Contract](#barrier-invocation-contract)
  - [Same-Type Nesting Admission](#same-type-nesting-admission-1)
  - [Mode-Conversion Pre-Approval Re-Evaluation](#mode-conversion-pre-approval-re-evaluation)
  - [GTS Availability Surface](#gts-availability-surface)
  - [Tenant-Type Envelope Alignment](#tenant-type-envelope-alignment)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-account-management-featstatus-tenant-type-enforcement`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-account-management-feature-tenant-type-enforcement`

## 1. Feature Context

### 1.1 Overview

Pre-write type-compatibility barrier invoked by every hierarchy-mutating write path inside `TenantService` before any `tenants` or `tenant_closure` row is created, and re-evaluated at mode-conversion approval time so an illegal parent/child topology cannot be introduced either at creation or via post-creation transitions. The barrier evaluates the GTS-registered `allowed_parent_types` compatibility matrix for the `tenant_type.v1` envelope and admits same-type nesting only when the GTS type definition permits it.

### 1.2 Purpose

Enforces parent-child tenant-type constraints against the runtime GTS types registry so the business hierarchy remains well-formed regardless of the deployment-specific topology (flat, cloud-hosting, education, enterprise). Keeping type enforcement isolated behind a reusable barrier lets the `tenant-hierarchy-management` create saga and the `managed-self-managed-modes` conversion approval flow share one authoritative classifier for `invalid_tenant_type` and `type_not_allowed` rejections instead of duplicating type logic inside each write path.

**Requirements**: `cpt-cf-account-management-fr-tenant-type-enforcement`, `cpt-cf-account-management-fr-tenant-type-nesting`

**Principles**: None

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-account-management-actor-tenant-admin` | Upstream caller whose hierarchy-mutating or mode-conversion-approval request ultimately triggers this barrier; never invokes the barrier directly — always via the owning saga or approval flow. |
| `cpt-cf-account-management-actor-gts-registry` | Runtime source of truth for registered tenant-type chained GTS identifiers and their `allowed_parent_types` trait values; consulted by this feature for every admit/reject decision. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.3 Tenant Type Enforcement (`fr-tenant-type-enforcement`, `fr-tenant-type-nesting`).
- **Design**: [DESIGN.md](../DESIGN.md) §3.1 Domain Model — Tenant Types GTS Schema with Traits (`tenant_type.v1~` envelope, `allowed_parent_types` trait, runtime registration + trait-driven validation).
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.3 Tenant Type Enforcement.
- **Dependencies**: `cpt-cf-account-management-feature-tenant-hierarchy-management` (the create saga is the primary caller of this barrier at step 3 `inst-algo-saga-type-check`; the mode-conversion flow in `cpt-cf-account-management-feature-managed-self-managed-modes` is the secondary caller at approval time), `cpt-cf-account-management-feature-errors-observability` (classification contract for `invalid_tenant_type`, `type_not_allowed`, and delegated GTS-unavailability errors).

## 2. Actor Flows (CDSL)

### Tenant Type Check on Child Create

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-tenant-type-enforcement-type-check-on-create`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- Child tenant type is a registered chained GTS identifier in the types registry, the requested parent type is present in the child type's `allowed_parent_types`, and any same-type nesting is explicitly admitted by the GTS trait — the barrier returns `admit` and the calling saga proceeds to the provisioning transaction.

**Error Scenarios**:

- Child `tenant_type` is not registered in GTS → barrier returns reject with `reason=INVALID_TENANT_TYPE`; the calling saga maps this to `CanonicalError::InvalidArgument` (HTTP 400) per the cross-cutting envelope.
- `parent_tenant_type` is not a member of the child type's `allowed_parent_types` → barrier returns reject with `reason=TYPE_NOT_ALLOWED`; the calling saga maps this to `CanonicalError::FailedPrecondition` (HTTP 400).
- Child and parent types are equal but the child type's `allowed_parent_types` does not include itself (same-type nesting not permitted) → barrier returns reject with `reason=TYPE_NOT_ALLOWED`.
- GTS is unreachable, times out, or cannot resolve the effective trait set → barrier returns the delegated `service_unavailable` classification without admitting or writing tenant state.

**Steps**:

1. [ ] - `p1` - Validate that the caller is the authorized `tenant-hierarchy-management` create saga step 3 (`inst-algo-saga-type-check`) for non-root creates and that the caller's `SecurityContext` is present on the invocation - `inst-flow-typchk-create-validate-caller`
2. [ ] - `p1` - Invoke `algo-allowed-parent-types-evaluation` with `(child_tenant_type, parent_tenant_type)` from the saga's validated create request - `inst-flow-typchk-create-invoke-algo`
3. [ ] - `p1` - **IF** algorithm returned `(error, category=ServiceUnavailable)` - `inst-flow-typchk-create-gts-unavailable`
   1. [ ] - `p1` - **RETURN** `(error, category=ServiceUnavailable)` so the saga can emit the delegated `CanonicalError::ServiceUnavailable` envelope entry with no DB side effects - `inst-flow-typchk-create-return-gts-unavailable`
4. [ ] - `p1` - **ELSE IF** algorithm returned `(reject, invalid_tenant_type)` - `inst-flow-typchk-create-reject-unregistered`
   1. [ ] - `p1` - **RETURN** `(reject, reason=INVALID_TENANT_TYPE)` so the saga can emit the `CanonicalError::InvalidArgument` envelope entry `inst-algo-saga-type-reject-return` - `inst-flow-typchk-create-return-invalid`
5. [ ] - `p1` - **ELSE IF** algorithm returned `(reject, type_not_allowed)` - `inst-flow-typchk-create-reject-not-allowed`
   1. [ ] - `p1` - **RETURN** `(reject, reason=TYPE_NOT_ALLOWED)` so the saga can emit the `CanonicalError::FailedPrecondition` envelope entry - `inst-flow-typchk-create-return-not-allowed`
6. [ ] - `p1` - **ELSE** algorithm returned `admit` - `inst-flow-typchk-create-admit`
   1. [ ] - `p1` - **RETURN** `admit` so the saga proceeds to `inst-algo-saga-depth-check` - `inst-flow-typchk-create-return-admit`

### Tenant Type Re-evaluation on Mode Conversion

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-tenant-type-enforcement-type-check-on-mode-conversion`

**Actor**: `cpt-cf-account-management-actor-tenant-admin`

**Success Scenarios**:

- At approval time for a pending managed/self-managed conversion, re-evaluation of the tenant's current `(child_tenant_type, parent_tenant_type)` against GTS confirms the topology is still legal after the mode flip, and the barrier returns `admit`; the approval flow in `managed-self-managed-modes` commits the conversion.

**Error Scenarios**:

- Between the conversion request and the conversion approval, the registered topology has shifted (GTS trait update, parent re-type) such that the current `parent_tenant_type` is no longer a member of the child type's `allowed_parent_types` → barrier returns reject with `reason=TYPE_NOT_ALLOWED`; the approval flow maps this to `CanonicalError::FailedPrecondition` (HTTP 400) and surfaces it to the caller. Per the conversion-flow contract owned by `managed-self-managed-modes`, the `ConversionRequest` row remains in `pending` (the type-check rejection does not auto-resolve the request); the conversion-flow's own retry / reject / cancel handling is what eventually transitions the row out of `pending`.
- Child tenant's registered `tenant_type` has been removed from GTS since the request was filed → barrier returns reject with `reason=INVALID_TENANT_TYPE`; the approval flow maps this to `CanonicalError::InvalidArgument` (HTTP 400).
- GTS is unreachable, times out, or cannot resolve effective traits at approval time → barrier returns the delegated `service_unavailable` classification and the approval transaction is not committed.

**Steps**:

1. [ ] - `p1` - Validate that the caller is the `managed-self-managed-modes` approval flow and that the caller's `SecurityContext` is present on the invocation - `inst-flow-typchk-conv-validate-caller`
2. [ ] - `p1` - Read current `child_tenant_type` and `parent_tenant_type` for the target tenant from `dbtable-tenants` (re-hydrated from Types Registry per DESIGN §3.1) - `inst-flow-typchk-conv-load-types`
3. [ ] - `p1` - Invoke `algo-allowed-parent-types-evaluation` with the freshly loaded `(child_tenant_type, parent_tenant_type)` - `inst-flow-typchk-conv-invoke-algo`
4. [ ] - `p1` - **IF** algorithm returned `(error, category=ServiceUnavailable)` - `inst-flow-typchk-conv-gts-unavailable`
   1. [ ] - `p1` - **RETURN** `(error, category=ServiceUnavailable)` to the approval flow for delegated envelope mapping; no mode flip is committed - `inst-flow-typchk-conv-return-gts-unavailable`
5. [ ] - `p1` - **ELSE IF** algorithm returned `(reject, invalid_tenant_type)` - `inst-flow-typchk-conv-reject-unregistered`
   1. [ ] - `p1` - **RETURN** `(reject, reason=INVALID_TENANT_TYPE)` to the approval flow for `CanonicalError::InvalidArgument` envelope mapping - `inst-flow-typchk-conv-return-invalid`
6. [ ] - `p1` - **ELSE IF** algorithm returned `(reject, type_not_allowed)` - `inst-flow-typchk-conv-reject-not-allowed`
   1. [ ] - `p1` - **RETURN** `(reject, reason=TYPE_NOT_ALLOWED)` to the approval flow for `CanonicalError::FailedPrecondition` envelope mapping - `inst-flow-typchk-conv-return-not-allowed`
7. [ ] - `p1` - **ELSE** algorithm returned `admit` - `inst-flow-typchk-conv-admit`
   1. [ ] - `p1` - **RETURN** `admit` so the approval flow can commit the mode flip - `inst-flow-typchk-conv-return-admit`

## 3. Processes / Business Logic (CDSL)

### Allowed Parent Types Evaluation

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-tenant-type-enforcement-allowed-parent-types-evaluation`

**Input**: `child_tenant_type` (chained GTS identifier under `gts.cf.core.am.tenant_type.v1~`), `parent_tenant_type` (chained GTS identifier for a non-root parent).

**Output**: `admit`, `(reject, reason=INVALID_TENANT_TYPE)`, `(reject, reason=TYPE_NOT_ALLOWED)`, or `(error, category=ServiceUnavailable)`.

**Steps**:

1. [ ] - `p1` - Probe `child_tenant_type` via `TypesRegistryClient` to confirm it is a registered chained GTS identifier and resolve its effective `allowed_parent_types` trait through `x-gts-traits` defaulting - `inst-algo-apte-probe-child`
2. [ ] - `p1` - **IF** GTS is unreachable, times out, or returns a trait-resolution failure before an effective trait value can be determined - `inst-algo-apte-gts-unavailable`
   1. [ ] - `p1` - **RETURN** `(error, category=ServiceUnavailable)` - `inst-algo-apte-return-gts-unavailable`
3. [ ] - `p1` - **IF** `child_tenant_type` is not registered in GTS or does not resolve under the `gts.cf.core.am.tenant_type.v1~` envelope - `inst-algo-apte-child-unregistered`
   1. [ ] - `p1` - **RETURN** `(reject, invalid_tenant_type)` - `inst-algo-apte-return-invalid-child`
4. [ ] - `p1` - **IF** the effective `allowed_parent_types` value is missing after default resolution or is not an array of chained tenant-type identifiers - `inst-algo-apte-trait-malformed`
   1. [ ] - `p1` - **RETURN** `(reject, invalid_tenant_type)` - `inst-algo-apte-return-malformed-trait`
5. [ ] - `p1` - **IF** `parent_tenant_type` is not a member of the effective `child_tenant_type.allowed_parent_types` - `inst-algo-apte-parent-not-allowed`
   1. [ ] - `p1` - **RETURN** `(reject, type_not_allowed)` - `inst-algo-apte-return-not-allowed`
6. [ ] - `p1` - **IF** `child_tenant_type` equals `parent_tenant_type` (same-type nesting requested) - `inst-algo-apte-same-type-branch`
   1. [ ] - `p1` - Invoke `algo-same-type-nesting-admission` with `child_tenant_type` and forward its decision - `inst-algo-apte-same-type-delegate`
   2. [ ] - `p1` - **RETURN** the forwarded decision (`admit` or `(reject, type_not_allowed)`) - `inst-algo-apte-return-same-type`
7. [ ] - `p1` - **RETURN** `admit` - `inst-algo-apte-return-admit`

### Same-Type Nesting Admission

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-tenant-type-enforcement-same-type-nesting-admission`

**Input**: `tenant_type` (chained GTS identifier under `gts.cf.core.am.tenant_type.v1~`).

**Output**: `admit`, `(reject, reason=TYPE_NOT_ALLOWED)`, or `(error, category=ServiceUnavailable)`.

**Steps**:

1. [ ] - `p1` - Resolve `tenant_type.allowed_parent_types` trait via `x-gts-traits` resolution against the GTS base schema `gts.cf.core.am.tenant_type.v1~` - `inst-algo-stn-resolve-trait`
2. [ ] - `p1` - **IF** GTS is unreachable, times out, or returns a trait-resolution failure before an effective trait value can be determined - `inst-algo-stn-gts-unavailable`
   1. [ ] - `p1` - **RETURN** `(error, category=ServiceUnavailable)` - `inst-algo-stn-return-gts-unavailable`
3. [ ] - `p1` - **IF** `tenant_type` is a member of its own effective `allowed_parent_types` (same-type nesting explicitly permitted by GTS) - `inst-algo-stn-self-allowed`
   1. [ ] - `p1` - **RETURN** `admit` - `inst-algo-stn-return-admit`
4. [ ] - `p1` - **ELSE** same-type nesting not permitted by the effective GTS trait - `inst-algo-stn-self-not-allowed`
   1. [ ] - `p1` - **RETURN** `(reject, type_not_allowed)` - `inst-algo-stn-return-reject`

## 4. States (CDSL)

**Not applicable.** This feature is a stateless pre-write barrier: it evaluates tenant-type compatibility synchronously against the GTS Types Registry and returns `admit` or `(reject, reason)` where `reason ∈ {INVALID_TENANT_TYPE, TYPE_NOT_ALLOWED}` without owning any persistent entity or lifecycle. The tenant-type compatibility matrix (`tenant_type.v1~` base schema and the `allowed_parent_types` trait) is declarative, owned by GTS, and consumed read-only by the evaluator algorithms; no state machine, no transitions, and no state IDs are emitted by this feature.

## 5. Definitions of Done

### Barrier Invocation Contract

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-type-enforcement-type-barrier-invocation-contract`

Every child-tenant create path and every mode-conversion approval path **MUST** invoke this barrier before any `tenants` or `tenant_closure` row is written or any mode flip is committed; illegal type pairings **MUST** be surfaced through the canonical envelope with `code=CanonicalError::InvalidArgument` + `reason=INVALID_TENANT_TYPE` (unregistered chained identifier) or `code=CanonicalError::FailedPrecondition` + `reason=TYPE_NOT_ALLOWED` (registered but disallowed pairing), and GTS unavailability **MUST** be surfaced as `code=CanonicalError::ServiceUnavailable`, with zero DB side-effects on every rejected/error path. The canonical category is the public stable `code`; `INVALID_TENANT_TYPE` / `TYPE_NOT_ALLOWED` travel as `reason` tokens on the field/precondition violation entries — there is no AM-private `code` surface. The barrier itself **MUST NOT** emit REST responses or audit entries — those surfaces are owned by the calling `tenant-hierarchy-management` create saga and the `managed-self-managed-modes` approval flow per the cross-cutting `errors-observability` envelope.

**Implements**:

- `cpt-cf-account-management-flow-tenant-type-enforcement-type-check-on-create`
- `cpt-cf-account-management-flow-tenant-type-enforcement-type-check-on-mode-conversion`
- `cpt-cf-account-management-algo-tenant-type-enforcement-allowed-parent-types-evaluation`
- `cpt-cf-account-management-algo-tenant-type-enforcement-same-type-nesting-admission`

**Constraints**: `cpt-cf-account-management-constraint-gts-availability`

**Touches**:

- Entities: `TenantType`, `AllowedParentTypes`
- Data: `gts://gts.cf.core.am.tenant_type.v1~` (GTS base schema for tenant types)
- Sibling integration: `cpt-cf-account-management-algo-tenant-hierarchy-management-create-tenant-saga` (step 3 `inst-algo-saga-type-check`)
- Error taxonomy: `errors-observability` envelope — public `code` is the canonical category (`CanonicalError::InvalidArgument` / `CanonicalError::FailedPrecondition` / `CanonicalError::ServiceUnavailable`); `INVALID_TENANT_TYPE` / `TYPE_NOT_ALLOWED` are `reason` tokens on field/precondition violations (catalog owned by `errors-observability`)

### Same-Type Nesting Admission

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-type-enforcement-same-type-nesting-admission`

Same-type nesting (child `tenant_type` equals parent `tenant_type`) **MUST** be admitted by the barrier if and only if the child type's `allowed_parent_types` trait contains the type's own chained GTS identifier; otherwise the barrier **MUST** reject with `reason=TYPE_NOT_ALLOWED`. The barrier **MUST NOT** attempt to detect or prevent cycles: acyclicity of the concrete `tenants` graph is a hierarchy invariant owned by `tenant-hierarchy-management` (the create saga refuses to insert a row whose parent chain would reach itself). Same-type admissibility is the only type-level question this DoD answers.

**Implements**:

- `cpt-cf-account-management-algo-tenant-type-enforcement-same-type-nesting-admission`
- `cpt-cf-account-management-flow-tenant-type-enforcement-type-check-on-create`

**Touches**:

- Entities: `TenantType`, `AllowedParentTypes`
- Data: `gts://gts.cf.core.am.tenant_type.v1~`

### Mode-Conversion Pre-Approval Re-Evaluation

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-type-enforcement-mode-conversion-preapproval-reevaluation`

The `managed-self-managed-modes` approval flow **MUST** re-invoke this barrier at approval time with the target tenant's freshly loaded `(child_tenant_type, parent_tenant_type)` so that GTS trait updates or parent re-types occurring between request and approval are caught; any flip that would yield an illegal topology **MUST** be rejected with `reason=TYPE_NOT_ALLOWED` (or `reason=INVALID_TENANT_TYPE` if the child type has been removed from GTS) before the mode change is committed. Re-evaluation **MUST** read current types at approval time — the request-time decision is not trusted for commit.

**Implements**:

- `cpt-cf-account-management-flow-tenant-type-enforcement-type-check-on-mode-conversion`
- `cpt-cf-account-management-algo-tenant-type-enforcement-allowed-parent-types-evaluation`

**Touches**:

- Entities: `TenantType`, `AllowedParentTypes`
- Sibling integration: `cpt-cf-account-management-feature-managed-self-managed-modes` approval flow (caller)

### GTS Availability Surface

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-type-enforcement-gts-availability-surface`

The barrier **MUST** treat the GTS Types Registry as a hard runtime dependency on the **unavailability path**: when GTS is unreachable, times out, or returns a resolution failure for a chained type identifier, the barrier **MUST** return `(error, category=ServiceUnavailable)` to the calling saga/flow for the cross-cutting `errors-observability` envelope rather than admitting blindly. AM **MUST NOT** cache type definitions locally for admit decisions; every barrier invocation re-resolves the type against GTS. This DoD is fully wired in `GtsTenantTypeChecker::check_parent_child`: a `tokio::time::timeout` (default 2 s) wraps the `TypesRegistryClient::get_type_schemas_by_uuid` call; transport errors and timeouts propagate as `DomainError::ServiceUnavailable`, boundary-converted to `CanonicalError::ServiceUnavailable` (HTTP 503).

**Implements**:

- `cpt-cf-account-management-algo-tenant-type-enforcement-allowed-parent-types-evaluation`

**Constraints**: `cpt-cf-account-management-constraint-gts-availability`

**Touches**:

- Error taxonomy: `errors-observability` envelope (catalog owned by `errors-observability`; classification of GTS-unavailability is delegated, not redefined)
- Data: `gts://gts.cf.core.am.tenant_type.v1~`

### Tenant-Type Envelope Alignment

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-tenant-type-enforcement-tenant-type-envelope-alignment`

The barrier **MUST** consume chained GTS identifiers whose base schema is `gts.cf.core.am.tenant_type.v1~` and **MUST** resolve the effective `allowed_parent_types` trait via the `x-gts-traits` resolution path per DESIGN §3.1. Trait values **MUST** be GTS-instance identifiers resolved to chained schema identifiers before comparison — string-equality on raw type names is not sufficient for admit decisions. Omitted trait properties in a derived type **MUST** fall back to defaults from the base `x-gts-traits-schema` (so an omitted `allowed_parent_types` resolves to `[]`); only a trait that is missing after effective resolution, non-array, or not composed of chained tenant-type identifiers **MUST** be treated as an unregistered child and rejected with `reason=INVALID_TENANT_TYPE`.

**Implements**:

- `cpt-cf-account-management-algo-tenant-type-enforcement-allowed-parent-types-evaluation`
- `cpt-cf-account-management-algo-tenant-type-enforcement-same-type-nesting-admission`

**Touches**:

- Entities: `TenantType`, `AllowedParentTypes`
- Data: `gts://gts.cf.core.am.tenant_type.v1~`
- DESIGN anchor: `DESIGN.md` §3.1 Tenant Types GTS Schema with Traits (envelope contract)

## 6. Acceptance Criteria

> **Implementation status.** The production `GtsTenantTypeChecker` (`gears/system/account-management/account-management/src/infra/types_registry/checker.rs`) implements the full `algo-allowed-parent-types-evaluation` and `algo-same-type-nesting-admission`: one batched `TypesRegistryClient::get_type_schemas_by_uuid` call resolves both schemas, `GtsTypeSchema::effective_traits()` produces the merged `allowed_parent_types` array (leaf-declared values win, then deepest-base defaults), and the membership check emits `INVALID_TENANT_TYPE` / `TYPE_NOT_ALLOWED` / `ServiceUnavailable` per the algorithm. The trait abstraction is wired into `TenantService::create_child` via `Arc<dyn TenantTypeChecker>` injected at construction. The `ClientHub` binding in the AM gear entry-point (`gear.rs`) is now in place: `TypesRegistryClient` is a hard runtime dependency. The entry-point declares `types-registry` in `#[toolkit::gear(deps = [...])]` so the runtime guarantees init ordering, then resolves the client from `ClientHub` and propagates a fatal error from `init` if the client cannot be obtained. On success it constructs `GtsTenantTypeChecker::new(client)` and passes it to `TenantService::new` (the same client is reused for the `tenant_type_uuid` → chained-id lookup that lowers `TenantModel` into the public `TenantInfo`). There is no production fallback to `InertTenantTypeChecker`; that checker exists for unit tests only, which construct `TenantService` directly via `inert_tenant_type_checker()` and bypass this init path. The ACs below describe the runtime contract; the algorithm itself is fully exercised by the unit tests in this PR.

- [ ] A child-tenant create request whose `tenant_type` is not a registered chained GTS identifier under `gts.cf.core.am.tenant_type.v1~` is rejected by the barrier with `reason=INVALID_TENANT_TYPE` (mapped to `validation` by the calling saga); no row is written to `dbtable-tenants` and no row is written or rewritten in `dbtable-tenant-closure`. Fingerprints `dod-tenant-type-enforcement-type-barrier-invocation-contract`.
- [ ] A child-tenant create request where the parent's `tenant_type` is not a member of the child type's `allowed_parent_types` trait is rejected by the barrier with `reason=TYPE_NOT_ALLOWED` (mapped to `conflict` by the calling saga); no row is written to `dbtable-tenants` and no row is written or rewritten in `dbtable-tenant-closure`. Fingerprints `dod-tenant-type-enforcement-type-barrier-invocation-contract`.
- [ ] Given a registered tenant type whose `allowed_parent_types` trait contains the type's own chained GTS identifier, a create request where `child_tenant_type == parent_tenant_type` is admitted by the barrier; given a registered tenant type whose `allowed_parent_types` does NOT include itself, the same same-type nesting request is rejected with `reason=TYPE_NOT_ALLOWED` (mapped to `conflict`). Fingerprints `dod-tenant-type-enforcement-same-type-nesting-admission`.
- [ ] At approval time for a pending `managed-self-managed-modes` conversion, if GTS trait updates or parent re-types since the request would produce an illegal topology, the barrier returns `reason=TYPE_NOT_ALLOWED` (mapped to `conflict` by the approval flow) and the mode flip is not committed; `dbtable-tenant-closure.barrier` for the target tenant is not rewritten. A complementary check confirms that when the topology remains legal, the barrier returns `admit` and the approval flow commits the conversion. Fingerprints `dod-tenant-type-enforcement-mode-conversion-preapproval-reevaluation`.
- [ ] When the GTS Types Registry is unreachable, times out, or returns a trait-resolution failure during a create or mode-conversion barrier invocation, the barrier returns `(error, category=ServiceUnavailable)` to the calling saga/approval flow; no tenant row, closure row, or mode flip is committed. Fingerprints `dod-tenant-type-enforcement-gts-availability-surface`.
- [ ] The barrier accepts full chained `GtsSchemaId` values whose base schema is `gts.cf.core.am.tenant_type.v1~` and resolves `allowed_parent_types` via `x-gts-traits` resolution per DESIGN §3.1; a create request whose `tenant_type` is a short-name alias or a chain whose base schema is not `gts.cf.core.am.tenant_type.v1~` is rejected with `reason=INVALID_TENANT_TYPE` (mapped to `validation`). A derived type that omits `allowed_parent_types` inherits the base default `[]`; a type whose effective trait is missing after resolution or is not an array of chained identifiers is rejected with `reason=INVALID_TENANT_TYPE`. A distinct chain whose leaf name collides with a different registered chain is NOT admitted solely by leaf-name equality. Fingerprints `dod-tenant-type-enforcement-tenant-type-envelope-alignment`.

## 7. Deliberate Omissions

- **Mode-conversion workflow, its state machine, and its REST surface (`ConversionRequest` dual-consent lifecycle)** — *Owned by `cpt-cf-account-management-feature-managed-self-managed-modes`* (DECOMPOSITION §2.4). This feature provides only the barrier re-evaluation at approval time; the approval flow, state transitions, and HTTP surface live there.
- **Authoring, publishing, or maintaining tenant-type definitions in GTS** — *Deployment-seeding concern, not an AM runtime responsibility.* Tenant types are registered via the GTS REST surface at deployment bootstrap; this feature consumes the registry read-only via `TypesRegistryClient` and does not write to GTS.
- **AuthZ read-path policy evaluation and barrier enforcement on reads** — *Owned by `PolicyEnforcer` / AuthZ Resolver / `tenant-resolver-plugin`* (out of this gear's write-path scope; the plugin feature is defined authoritatively in the `cf-tr-plugin` sub-system DECOMPOSITION and referenced from AM DECOMPOSITION §2.10). This feature is a pre-write barrier on the write path only; it does not participate in read-time policy evaluation or the barrier-mode reductions applied to queries.
- **Tenant creation, update, soft-delete, hard-delete, and closure maintenance** — *Owned by `cpt-cf-account-management-feature-tenant-hierarchy-management`* (DECOMPOSITION §2.2). This feature only validates type compatibility and returns `admit` / `(reject, reason)` (`reason ∈ {INVALID_TENANT_TYPE, TYPE_NOT_ALLOWED}`); it does not write `dbtable-tenants` or `dbtable-tenant-closure` and does not maintain hierarchy invariants (acyclicity, depth, closure integrity).
- **Cross-cutting error taxonomy, RFC 9457 envelope, audit pipeline, reliability / SLA policy, and metric catalog naming-alignment contract** — *Owned by `cpt-cf-account-management-feature-errors-observability`* (DECOMPOSITION §2.8). The public `code` identifiers (`invalid_tenant_type`, `type_not_allowed`, `service_unavailable`) and the GTS-unavailability classification are catalogued there; this feature emits codes by name and defers envelope formatting, HTTP status mapping, audit emission, and metric sample naming to that feature.
- **`ClientHub` binding for the production `GtsTenantTypeChecker`** — *Wired in the AM gear entry-point (`gear.rs`).* The production `GtsTenantTypeChecker` (`gears/system/account-management/account-management/src/infra/types_registry/checker.rs`) implements the full algorithm against `types_registry_sdk::TypesRegistryClient`: one `get_type_schemas_by_uuid([child, parent])` round-trip, `GtsTypeSchema::effective_traits()` merge across the inheritance chain (leaf-declared trait values win, then deepest-base defaults from `x-gts-traits-schema`), and a string-equality membership check on the resolved `allowed_parent_types` array against the parent's chained `type_id`. Same-type nesting is admitted iff the type's own chained identifier appears in its own `allowed_parent_types`. Errors map cleanly: GTS unreachable / timeout / unrecognised registry error → `DomainError::ServiceUnavailable`; child or parent UUID not registered → `DomainError::InvalidTenantType`; effective trait missing or non-array → `DomainError::InvalidTenantType`; pairing rejected → `DomainError::TypeNotAllowed`. The wire-up is now in place: `TypesRegistryClient` is treated as a hard runtime dependency. The AM gear entry-point declares `types-registry` in `#[toolkit::gear(deps = [...])]` (the runtime guarantees init ordering), resolves the client from `ClientHub`, and fails `init` with a propagated error if the client cannot be obtained — there is no production fallback to `InertTenantTypeChecker`. On success the entry-point binds `Arc::new(GtsTenantTypeChecker::new(client))` and reuses the same client for the `tenant_type_uuid` → chained-id lookup on every service-layer CRUD return value. `InertTenantTypeChecker` is reserved for unit tests, which construct `TenantService` directly via `inert_tenant_type_checker()` and bypass this init path.
- **No dedicated REST API surface, no dedicated sequence diagram, no new Design Components** — *Per DECOMPOSITION §2.3 scope* (`API: none`, `Sequences: none`, `Design Components: none`). Enforcement is an internal pre-write barrier co-located inside `TenantService`, invoked as step 3 `inst-algo-saga-type-check` of `algo-tenant-hierarchy-management-create-tenant-saga` (owned by `tenant-hierarchy-management`) and at approval time of the `managed-self-managed-modes` conversion flow; the barrier surface is a method contract, not a new component, endpoint, or top-level sequence.
