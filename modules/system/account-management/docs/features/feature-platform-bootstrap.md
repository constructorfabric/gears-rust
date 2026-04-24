# Feature: Platform Bootstrap


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Platform Bootstrap Saga](#platform-bootstrap-saga)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Idempotency Detection](#idempotency-detection)
  - [IdP Availability Wait with Exponential Backoff](#idp-availability-wait-with-exponential-backoff)
  - [Root-Tenant Finalization Saga](#root-tenant-finalization-saga)
- [4. States (CDSL)](#4-states-cdsl)
  - [Root Tenant Bootstrap Lifecycle](#root-tenant-bootstrap-lifecycle)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement Root Tenant Auto-Creation](#implement-root-tenant-auto-creation)
  - [Implement Root Tenant IdP Linking](#implement-root-tenant-idp-linking)
  - [Implement Bootstrap Idempotency](#implement-bootstrap-idempotency)
  - [Implement IdP Wait Ordering](#implement-idp-wait-ordering)
  - [Implement Bootstrap Audit and Metrics Emission](#implement-bootstrap-audit-and-metrics-emission)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-account-management-featstatus-platform-bootstrap`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-account-management-feature-platform-bootstrap`
## 1. Feature Context

### 1.1 Overview

AM automatically creates the initial root tenant on first platform start, links it to the configured IdP provider, and blocks the module ready-signal until both succeed. Idempotent across restarts and platform upgrades; a stale `provisioning` row left by a prior failed attempt defers to the Provisioning Reaper rather than being retried in place.

### 1.2 Purpose

Implements PRD §5.1 Platform Bootstrap — the foundation FR group without which no tenant hierarchy can exist. The feature owns the one-time wiring moment at `AccountManagementModule` lifecycle entry: distributed-lock acquisition, idempotency detection against the existing tenants table, IdP availability wait with exponential backoff, and the three-step saga that creates the root tenant row, invokes `provision_tenant` on the IdP contract, and finalizes status + closure self-row atomically.

**Requirements**: `cpt-cf-account-management-fr-root-tenant-creation`, `cpt-cf-account-management-fr-root-tenant-idp-link`, `cpt-cf-account-management-fr-bootstrap-idempotency`, `cpt-cf-account-management-fr-bootstrap-ordering`

**Principles**: `cpt-cf-account-management-principle-source-of-truth` (bootstrap establishes the root tenant as the canonical hierarchy anchor), `cpt-cf-account-management-principle-idp-agnostic` (bootstrap is the first invocation of the IdP pluggable contract).

> **Note on DECOMPOSITION alignment**: this FEATURE claims the two principles above because bootstrap is the feature that first instantiates each one; `DECOMPOSITION.md` §2.1 lists the same principles under `Design Principles Covered`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-account-management-actor-platform-admin` | Configures bootstrap parameters (`root_tenant_type`, `root_tenant_name`, `root_tenant_metadata`, IdP retry/timeout values) before platform start; observes bootstrap outcome via audit + metrics. |
| `cpt-cf-account-management-actor-idp` | Receives `provision_tenant(root_id, ...)` during the saga; returns provisioning metadata that AM persists as `tenant_metadata` entries. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.1 Platform Bootstrap
- **Design**: [DESIGN.md](../DESIGN.md) §3.2 `BootstrapService` + `AccountManagementModule`, §3.6 `seq-bootstrap`
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.1 Platform Bootstrap
- **Dependencies**: `cpt-cf-account-management-feature-errors-observability` (error taxonomy + bootstrap-lifecycle metric family + `actor=system` audit contract)

## 2. Actor Flows (CDSL)

Bootstrap is triggered by the `AccountManagementModule` lifecycle rather than an end-user request. The flow below traces the indirect actor path: Platform Administrator's deployment configuration drives ModKit's `lifecycle(entry = ...)` invocation, which in turn drives `BootstrapService`.

### Platform Bootstrap Saga

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-platform-bootstrap-saga`

**Actor**: `cpt-cf-account-management-actor-platform-admin`

**Success Scenarios**:

- First platform start: root tenant is created with configured `root_tenant_type`, IdP binding established, status transitions `provisioning → active`, `tenant_closure` self-row present, module signals ready.
- Restart after prior success: idempotent detection finds the existing `active` root; saga is skipped; module signals ready immediately after lock release.
- IdP briefly unavailable at start: wait-loop backs off and eventually proceeds when IdP reports available within the configured total timeout.

**Error Scenarios**:

- IdP never becomes available within total timeout: bootstrap fails with `idp_unavailable`; no `provisioning` row is left behind; module does not signal ready.
- Root tenant type preflight fails: the configured root type is not registered in GTS, GTS is unavailable, or the effective `allowed_parent_types` value is not root-eligible; no `tenants` row is written.
- Finalization fails after successful `provision_tenant`: `tenants` row remains in `provisioning`; Provisioning Reaper compensates on its next sweep; next bootstrap attempt recreates the root.
- Concurrent replicas race: only one holder of the bootstrap lock executes the saga; other replicas wait and observe the completed state on lock release.

**Steps**:

1. [ ] - `p1` - ModKit invokes `AccountManagementModule.lifecycle(entry = ...)` — `inst-flow-bootstrap-lifecycle-entry`
2. [ ] - `p1` - Module calls `BootstrapService.run(bootstrap_config)` before signalling module ready - `inst-flow-bootstrap-invoke-service`
3. [ ] - `p1` - Increment `bootstrap.attempts` counter (per attempt, before any lock or DB work) - `inst-flow-bootstrap-metric-attempt`
4. [ ] - `p1` - Acquire bootstrap distributed lock (implementation-specific: DB advisory lock or external lock service, see DESIGN §3.6) - `inst-flow-bootstrap-acquire-lock`
5. [ ] - `p1` - Query for existing root tenant via TenantService.find_root() - `inst-flow-bootstrap-detect-root`
6. [ ] - `p1` - Run idempotency classification via `algo-platform-bootstrap-idempotency-detection` over the result - `inst-flow-bootstrap-classify-idempotency`
7. [ ] - `p1` - **IF** classification = `active-root-exists` - `inst-flow-bootstrap-branch-active`
   1. [ ] - `p1` - Emit audit event `bootstrap.skipped` with `actor=system` - `inst-flow-bootstrap-audit-skipped`
   2. [ ] - `p1` - Emit `bootstrap.outcome` counter labeled `classification=skipped` - `inst-flow-bootstrap-metric-outcome-skipped`
   3. [ ] - `p1` - Release lock - `inst-flow-bootstrap-release-lock-skip`
   4. [ ] - `p1` - **RETURN** Bootstrap skipped (idempotent) - `inst-flow-bootstrap-return-skip`
8. [ ] - `p1` - **IF** classification = `provisioning-root-stuck` - `inst-flow-bootstrap-branch-stuck`
   1. [ ] - `p1` - Emit audit event `bootstrap.deferred-to-reaper` with `actor=system` - `inst-flow-bootstrap-audit-deferred`
   2. [ ] - `p1` - Emit `bootstrap.outcome` counter labeled `classification=deferred-to-reaper` - `inst-flow-bootstrap-metric-outcome-deferred`
   3. [ ] - `p1` - Release lock without creating a second root - `inst-flow-bootstrap-release-lock-stuck`
   4. [ ] - `p1` - **RETURN** Bootstrap not complete; await reaper compensation - `inst-flow-bootstrap-return-stuck`
9. [ ] - `p1` - **IF** classification = `invariant-violation` (suspended or deleted status on root row — illegal pre-existing state) - `inst-flow-bootstrap-branch-invariant`
   1. [ ] - `p1` - Emit audit event `bootstrap.invariant-violation` with `actor=system` + observed-status detail - `inst-flow-bootstrap-audit-invariant`
   2. [ ] - `p1` - Emit `bootstrap.outcome` counter labeled `classification=invariant-violation` - `inst-flow-bootstrap-metric-outcome-invariant`
   3. [ ] - `p1` - Release lock without creating a second root - `inst-flow-bootstrap-release-lock-invariant`
   4. [ ] - `p1` - **RETURN** `invariant_violation` error (root in illegal state — manual intervention required) - `inst-flow-bootstrap-return-invariant`
10. [ ] - `p1` - **ELSE** (classification = `no-root`; proceed to create root) - `inst-flow-bootstrap-branch-create`
    1. [ ] - `p1` - Wait for IdP availability via `algo-platform-bootstrap-idp-wait-with-backoff` - `inst-flow-bootstrap-wait-idp`
    2. [ ] - `p1` - **IF** IdP wait exhausted without success - `inst-flow-bootstrap-idp-timeout`
       1. [ ] - `p1` - Emit audit event `bootstrap.idp-timeout` with `actor=system` - `inst-flow-bootstrap-audit-idp-timeout`
       2. [ ] - `p1` - Emit `bootstrap.outcome` counter labeled `classification=idp-timeout` - `inst-flow-bootstrap-metric-outcome-idp-timeout`
       3. [ ] - `p1` - Release lock - `inst-flow-bootstrap-release-lock-idp-timeout`
       4. [ ] - `p1` - **RETURN** `idp_unavailable` - `inst-flow-bootstrap-return-idp-timeout`
    3. [ ] - `p1` - Execute `algo-platform-bootstrap-finalization-saga` against `TenantService.create_root_tenant(bootstrap_config)` - `inst-flow-bootstrap-run-finalization`
    4. [ ] - `p1` - **IF** finalization succeeded - `inst-flow-bootstrap-finalize-success`
       1. [ ] - `p1` - Emit audit event `bootstrap.completed` with `actor=system` - `inst-flow-bootstrap-audit-completed`
       2. [ ] - `p1` - Emit `bootstrap.outcome` counter labeled `classification=completed` - `inst-flow-bootstrap-metric-outcome-completed`
       3. [ ] - `p1` - Release lock - `inst-flow-bootstrap-release-lock-ok`
       4. [ ] - `p1` - **RETURN** Bootstrap complete - `inst-flow-bootstrap-return-ok`
    5. [ ] - `p1` - **ELSE** finalization returned `clean_failure` or `ambiguous_failure` - `inst-flow-bootstrap-finalize-fail`
       1. [ ] - `p1` - Emit audit event `bootstrap.finalization-failed` with `actor=system` + failure reason and failure class - `inst-flow-bootstrap-audit-failed`
       2. [ ] - `p1` - Emit `bootstrap.outcome` counter labeled `classification=clean-failure` or `classification=ambiguous-failure` - `inst-flow-bootstrap-metric-outcome-failed`
       3. [ ] - `p1` - Release lock - `inst-flow-bootstrap-release-lock-fail`
       4. [ ] - `p1` - **IF** finalization returned `clean_failure` - `inst-flow-bootstrap-clean-failure`
          1. [ ] - `p1` - **RETURN** Bootstrap not complete; no root state remains and a later retry may run the saga again - `inst-flow-bootstrap-return-clean-fail`
       5. [ ] - `p1` - **ELSE** finalization returned `ambiguous_failure` - `inst-flow-bootstrap-ambiguous-failure`
          1. [ ] - `p1` - **RETURN** Bootstrap not complete; root remains in `provisioning` for reaper compensation - `inst-flow-bootstrap-return-ambiguous-fail`

## 3. Processes / Business Logic (CDSL)

### Idempotency Detection

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-platform-bootstrap-idempotency-detection`

**Input**: Result of `TenantService.find_root()` — zero or one tenant record (id, status)

**Output**: Classification enum — `active-root-exists`, `provisioning-root-stuck`, `no-root`, or `invariant-violation`

**Steps**:

1. [ ] - `p1` - Parse the query result into `{ row_count, first_status }` - `inst-algo-idem-parse`
2. [ ] - `p1` - **IF** `row_count == 0` - `inst-algo-idem-no-row`
   1. [ ] - `p1` - **RETURN** `no-root` - `inst-algo-idem-return-no-root`
3. [ ] - `p1` - **IF** `first_status == 1` (active) - `inst-algo-idem-row-active`
   1. [ ] - `p1` - **RETURN** `active-root-exists` - `inst-algo-idem-return-active`
4. [ ] - `p1` - **IF** `first_status == 0` (provisioning) - `inst-algo-idem-row-provisioning`
   1. [ ] - `p1` - **RETURN** `provisioning-root-stuck` - `inst-algo-idem-return-stuck`
5. [ ] - `p1` - **ELSE** unexpected status (suspended or deleted on a root row) - `inst-algo-idem-invariant-violation`
   1. [ ] - `p1` - **RETURN** `invariant-violation` (fail-fast — root cannot be suspended or deleted) - `inst-algo-idem-return-invariant`

### IdP Availability Wait with Exponential Backoff

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-platform-bootstrap-idp-wait-with-backoff`

**Input**: `bootstrap_config` containing `idp_retry_backoff_initial` (default 2s), `idp_retry_backoff_max` (default 30s), `idp_retry_timeout` (default 5min)

**Output**: Result — `available` (IdP reported ready) or `timeout` (total timeout exceeded)

**Steps**:

1. [ ] - `p1` - Initialize `current_backoff = idp_retry_backoff_initial`; `elapsed = 0` - `inst-algo-wait-init`
2. [ ] - `p1` - Record start timestamp for elapsed-time calculation - `inst-algo-wait-start-timer`
3. [ ] - `p1` - **TRY** `IdpProviderPluginClient::check_availability()` using the configured timeout budget - `inst-algo-wait-try-probe`
   1. [ ] - `p1` - **IF** IdP reports available - `inst-algo-wait-probe-ok`
      1. [ ] - `p1` - Emit metric `bootstrap.idp_wait.duration` with observed `elapsed` - `inst-algo-wait-metric-ok`
      2. [ ] - `p1` - **RETURN** `available` - `inst-algo-wait-return-ok`
4. [ ] - `p1` - **CATCH** any IdP error or unavailability - `inst-algo-wait-catch`
   1. [ ] - `p1` - **IF** `elapsed >= idp_retry_timeout` - `inst-algo-wait-check-timeout`
      1. [ ] - `p1` - Emit metric `bootstrap.idp_wait.timeout` (counter) - `inst-algo-wait-metric-timeout`
      2. [ ] - `p1` - **RETURN** `timeout` - `inst-algo-wait-return-timeout`
   2. [ ] - `p1` - Sleep for `current_backoff` - `inst-algo-wait-sleep`
   3. [ ] - `p1` - Update elapsed from recorded start - `inst-algo-wait-update-elapsed`
   4. [ ] - `p1` - Double `current_backoff`, capping at `idp_retry_backoff_max` - `inst-algo-wait-grow-backoff`
   5. [ ] - `p1` - Retry from the IdP availability-check step - `inst-algo-wait-retry`

### Root-Tenant Finalization Saga

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-platform-bootstrap-finalization-saga`

**Input**: `bootstrap_config` (with resolved `root_tenant_type`, `root_tenant_name`, `root_tenant_metadata`)

**Output**: Result — `success` (root tenant visible in `active` status with self-closure row), `clean_failure` (no AM or IdP root state retained; safe to retry), or `ambiguous_failure` (provisioning row persists, awaits reaper)

**Steps**:

> This algorithm describes the saga at the `TenantService` abstraction level. `TenantService` owns the concrete DB operations (schema layout, column-level ORM calls, transaction boundaries); the algo specifies *which* service methods are invoked and in what order, not *how* they are implemented. See DESIGN §3.2 `TenantService` component + §3.6 `seq-bootstrap` for the authoritative DB-level contract.

1. [ ] - `p1` - Resolve `bootstrap_config.root_tenant_type` through `TypesRegistryClient` using DESIGN §3.1 effective-trait resolution; this bootstrap-owned root preflight does not call downstream barrier features because bootstrap is earlier in the feature DAG - `inst-algo-saga-type-check`
2. [ ] - `p1` - **IF** GTS is unavailable, times out, or cannot resolve effective traits - `inst-algo-saga-type-gts-unavailable`
   1. [ ] - `p1` - **RETURN** `clean_failure` with the delegated `service_unavailable` classification from `errors-observability`; no DB state persisted and no IdP call issued - `inst-algo-saga-return-gts-unavailable`
3. [ ] - `p1` - **IF** the configured root type is not a registered chained tenant type under `gts.x.core.am.tenant_type.v1~` - `inst-algo-saga-type-invalid-branch`
   1. [ ] - `p1` - **RETURN** `clean_failure` with `sub_code=invalid_tenant_type` (mapped to `validation` by the `errors-observability` envelope); no DB state persisted - `inst-algo-saga-return-invalid-type`
4. [ ] - `p1` - **ELSE IF** the effective `allowed_parent_types` trait is not exactly `[]` after default resolution - `inst-algo-saga-type-not-root-branch`
   1. [ ] - `p1` - **RETURN** `clean_failure` with `sub_code=type_not_allowed` (mapped to `conflict` by the `errors-observability` envelope); no DB state persisted - `inst-algo-saga-return-not-root-type`
5. [ ] - `p1` - **TRY** saga step 1 (short TX, TenantService-owned) - `inst-algo-saga-step-1`
   1. [ ] - `p1` - TenantService: insert root tenant row in `provisioning` status (no parent, depth 0, not self-managed, resolved type uuid) and commit the transaction - `inst-algo-saga-insert-provisioning`
6. [ ] - `p1` - **CATCH** saga step 1 error - `inst-algo-saga-step-1-catch`
   1. [ ] - `p1` - **RETURN** `clean_failure` (no row persisted; no cleanup needed) - `inst-algo-saga-return-step-1-fail`
7. [ ] - `p1` - **TRY** saga step 2 (IdP call, no open TX) - `inst-algo-saga-step-2`
   1. [ ] - `p1` - IdP: `provision_tenant(root_id, root_tenant_name, root_tenant_type, root_tenant_metadata)` - `inst-algo-saga-idp-call`
   2. [ ] - `p1` - Receive `ProvisionResult` (may include zero or more provider-supplied metadata entries) - `inst-algo-saga-receive-result`
8. [ ] - `p1` - **CATCH** saga step 2 error - `inst-algo-saga-step-2-catch`
   1. [ ] - `p1` - **IF** the provider result proves no IdP-side root state was retained - `inst-algo-saga-step-2-clean-branch`
      1. [ ] - `p1` - TenantService: delete the `provisioning` root row in a short compensating transaction; **RETURN** `clean_failure` with `sub_code=idp_unavailable` and safe retry semantics - `inst-algo-saga-return-step-2-clean`
   2. [ ] - `p1` - **ELSE** the external outcome is ambiguous or may already be retained by the IdP - `inst-algo-saga-step-2-ambiguous-branch`
      1. [ ] - `p1` - **RETURN** `ambiguous_failure` (provisioning row left for reaper to compensate per seq-bootstrap; caller must reconcile before blind retry) - `inst-algo-saga-return-step-2-ambiguous`
9. [ ] - `p1` - **TRY** saga step 3 (finalize, short TX, TenantService-owned) - `inst-algo-saga-step-3`
   1. [ ] - `p1` - TenantService: persist each provider-returned `ProvisionResult` metadata entry (GTS-validated), transition root status to `active`, and insert the root's self-row in `tenant_closure` (`ancestor = descendant = root_id`, barrier = 0, descendant_status = active) — all in a single transaction - `inst-algo-saga-finalize`
   2. [ ] - `p1` - **RETURN** `success` - `inst-algo-saga-return-success`
10. [ ] - `p1` - **CATCH** saga step 3 error (e.g. metadata schema not registered, constraint violation) - `inst-algo-saga-step-3-catch`
   1. [ ] - `p1` - **RETURN** `ambiguous_failure` (provisioning row left for reaper; IdP-side provisioning will be compensated via `deprovision_tenant` by the reaper) - `inst-algo-saga-return-step-3-fail`

## 4. States (CDSL)

### Root Tenant Bootstrap Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-account-management-state-platform-bootstrap-root-tenant-status`

**States**: `absent`, `provisioning`, `active`, `stuck-provisioning`

**Initial State**: `absent`

**State Semantics**:

- `absent` — no row with `parent_id IS NULL` in `tenants`
- `provisioning` — `tenants.status = 0` (SMALLINT); saga in-flight; no `tenant_closure` row
- `active` — `tenants.status = 1` (SMALLINT); self-row present in `tenant_closure`
- `stuck-provisioning` — `tenants.status = 0` observed on a subsequent bootstrap invocation (saga did not finalize on prior start); reaper compensates

**Transitions**:

1. [ ] - `p1` - **FROM** `absent` **TO** `provisioning` **WHEN** saga step 1 commits and creates the root tenant row in provisioning status - `inst-state-absent-to-provisioning`
2. [ ] - `p1` - **FROM** `provisioning` **TO** `active` **WHEN** saga step 3 commits and finalizes the root tenant with its closure self-row - `inst-state-provisioning-to-active`
3. [ ] - `p1` - **FROM** `provisioning` **TO** `stuck-provisioning` **WHEN** bootstrap process observes the `provisioning` row on a re-entry (prior saga did not complete step 3) - `inst-state-provisioning-to-stuck`
4. [ ] - `p1` - **FROM** `stuck-provisioning` **TO** `absent` **WHEN** Provisioning Reaper deletes the row after `deprovision_tenant` cleanup - `inst-state-stuck-to-absent`
5. [ ] - `p1` - **FROM** `absent` (post-reaper) **TO** `provisioning` **WHEN** a later bootstrap attempt starts the saga again - `inst-state-retry-after-reaper`

## 5. Definitions of Done

### Implement Root Tenant Auto-Creation

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-platform-bootstrap-root-creation`

The system **MUST** create exactly one root tenant row (`parent_id IS NULL`) during the first successful bootstrap, finalize it to `active` status, and write the corresponding self-row in `tenant_closure`. Bootstrap **MUST NOT** expose a root tenant in `active` status until the three-step saga has fully committed.

**Implements**:

- `cpt-cf-account-management-flow-platform-bootstrap-saga`
- `cpt-cf-account-management-algo-platform-bootstrap-finalization-saga`
- `cpt-cf-account-management-state-platform-bootstrap-root-tenant-status`

**Touches**:

- DB: `tenants`, `tenant_closure`, `tenant_metadata`
- Entities: `Tenant`, `TenantClosure`

### Implement Root Tenant IdP Linking

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-platform-bootstrap-idp-linking`

The system **MUST** invoke the IdP provider's `provision_tenant(root_id, root_tenant_name, root_tenant_type, root_tenant_metadata)` exactly once during a successful bootstrap and persist every metadata entry returned in `ProvisionResult` as a `tenant_metadata` row (validated against registered GTS schemas). Bootstrap **MUST NOT** validate or interpret `root_tenant_metadata` content — it is a pass-through forwarded as-is.

**Implements**:

- `cpt-cf-account-management-algo-platform-bootstrap-finalization-saga`

**Constraints**: `cpt-cf-account-management-principle-idp-agnostic`

**Touches**:

- DB: `tenant_metadata`
- Entities: `Tenant`, `TenantMetadataEntry`

### Implement Bootstrap Idempotency

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-platform-bootstrap-idempotency`

The system **MUST** detect an existing active root tenant on platform restart or upgrade and complete bootstrap as a no-op. When a `provisioning` root row is observed (stuck from a prior failed attempt), bootstrap **MUST** defer to the Provisioning Reaper and **MUST NOT** create a second root or re-run the saga against the stale row.

**Implements**:

- `cpt-cf-account-management-flow-platform-bootstrap-saga`
- `cpt-cf-account-management-algo-platform-bootstrap-idempotency-detection`

**Touches**:

- DB: `tenants`
- Entities: `Tenant`

### Implement IdP Wait Ordering

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-platform-bootstrap-idp-wait-ordering`

The system **MUST** block bootstrap at `IdpProviderPluginClient::check_availability()` until the IdP reports available, using exponential backoff with the configured `idp_retry_backoff_initial` (default 2s), capped at `idp_retry_backoff_max` (default 30s), and bounded by `idp_retry_timeout` (default 5min). On timeout, bootstrap **MUST** return `idp_unavailable` and leave no partial `provisioning` row behind.

**Implements**:

- `cpt-cf-account-management-algo-platform-bootstrap-idp-wait-with-backoff`

**Touches**:

- External contract: IdP provider plugin (`check_availability`)
- Metrics: `bootstrap.idp_wait.duration`, `bootstrap.idp_wait.timeout`

### Implement Bootstrap Audit and Metrics Emission

- [ ] `p1` - **ID**: `cpt-cf-account-management-dod-platform-bootstrap-audit-and-metrics`

The system **MUST** emit `actor=system` platform audit events at every terminal bootstrap outcome (`bootstrap.completed`, `bootstrap.skipped`, `bootstrap.deferred-to-reaper`, `bootstrap.finalization-failed`) and **MUST** export the bootstrap-lifecycle metric family (attempt counter, IdP-wait duration histogram, IdP-wait timeout counter, outcome counter by terminal classification) through the module's observability plumbing owned by the errors-observability feature.

**Implements**:

- `cpt-cf-account-management-flow-platform-bootstrap-saga`
- `cpt-cf-account-management-algo-platform-bootstrap-idp-wait-with-backoff`

**Constraints**: Metric names and audit schema are anchored by the `errors-observability` feature's catalog; this feature contributes entries but does not redefine the catalog.

**Touches**:

- Platform audit sink
- Metrics: `bootstrap.attempts`, `bootstrap.outcome{classification}`, `bootstrap.idp_wait.duration`, `bootstrap.idp_wait.timeout`

## 6. Acceptance Criteria

- [ ] First platform start: root tenant row exists with `status = 1` (active), `parent_id IS NULL`, `depth = 0`; `tenant_closure` contains exactly one row `(root_id, root_id, 0, 1)`; module signals ready; audit sink has a `bootstrap.completed actor=system` event.
- [ ] Second platform start (post-success): no new `tenants` row is created; no second `provision_tenant` call is issued; audit sink has a `bootstrap.skipped actor=system` event; module signals ready.
- [ ] Start observing a `provisioning` root row (prior saga not finalized): no second root created; bootstrap logs defer-to-reaper outcome; audit sink has a `bootstrap.deferred-to-reaper actor=system` event; after successful Provisioning Reaper compensation, a subsequent start recreates the root through the full saga.
- [ ] IdP unavailable for longer than `idp_retry_timeout` during `check_availability`: bootstrap returns `idp_unavailable`; no `tenants` row is left in `provisioning`; `bootstrap.idp_wait.timeout` metric is incremented; module does not signal ready.
- [ ] Concurrent replica starts on a fresh database: exactly one replica creates the root; other replicas observe the completed state on lock release and return `bootstrap.skipped`; no duplicate `tenants` or `tenant_closure` rows exist.
- [ ] Bootstrap configuration with `root_tenant_type` that is not registered in GTS: the bootstrap-owned root-type preflight returns `clean_failure` with `sub_code=invalid_tenant_type` **before** saga step 1 begins; no `tenants` row is written; no IdP call is issued. A configuration whose registered `root_tenant_type` has an effective `allowed_parent_types` value other than `[]` fails the same way with `sub_code=type_not_allowed`; a GTS transport/timeout failure returns the delegated `service_unavailable` classification with no DB side effects.
- [ ] During `provision_tenant`, a provider failure that proves no IdP-side root state was retained deletes the `provisioning` row in a compensating transaction and returns `clean_failure` with `sub_code=idp_unavailable`; the next bootstrap retry may safely re-run the saga. A transport timeout or ambiguous provider result leaves the `provisioning` row for the reaper, returns `ambiguous_failure`, and does not invite blind automatic retry.
- [ ] Start observing a suspended or deleted root tenant row (illegal pre-existing state): bootstrap returns `invariant_violation`; no second root is created; module does not signal ready; audit sink has a `bootstrap.invariant-violation actor=system` event.

## 7. Deliberate Omissions

The following concerns are explicitly **not** addressed by this FEATURE. Each is recorded so reviewers can distinguish intentional exclusion (author considered and excluded with reasoning) from accidental omission.

- **UX / usability** — *Not applicable.* Bootstrap is a system-internal lifecycle operation triggered by ModKit module startup; it has no user-facing interface, no user input, and no interaction surface. Observability for operators (audit + metrics) is covered by §5.5 and delegated to `errors-observability`.
- **Regulatory compliance / data-subject rights** — *Not applicable.* Bootstrap creates no user data, collects no consent, and has no retention or data-subject-rights surface. The only data written is AM-internal structural rows (root tenant, closure self-row, optional provider metadata).
- **Data privacy (PII)** — *Not applicable.* `root_tenant_metadata` is an opaque deployment-configuration blob that AM forwards as-is to the IdP provider plugin without interpretation, and any `ProvisionResult` metadata entries AM persists are provider-returned and validated against GTS-registered schemas — AM neither introspects nor normalizes them, which keeps bootstrap out of any PII-handling boundary.
- **Distributed-lock implementation choice** — *Not prescribed by this FEATURE.* DESIGN §3.6 explicitly leaves the lock implementation open ("infrastructure-specific — database advisory lock, distributed lock service, or equivalent"). No DoD in §5 mandates a specific lock mechanism; the flow's lock steps are implementation-agnostic.
- **Concrete metric names and audit-event schemas** — *Owned by `errors-observability`.* This FEATURE references the `bootstrap.*` metric family and `bootstrap.*` audit-event names by stable label but does not define their carrier schema, cardinality limits, or retention; those contracts live in the errors-observability FEATURE's metric catalog and audit-event registry.
- **Conforming IdP plugin implementations** — *Out of scope.* The pluggable IdP contract is referenced via `provision_tenant` but the individual provider plugins (Keycloak, custom IdPs, etc.) are separate crates owned by the `idp-user-operations-contract` feature; this FEATURE tests only that the contract is invoked correctly and the ProvisionResult is persisted.
