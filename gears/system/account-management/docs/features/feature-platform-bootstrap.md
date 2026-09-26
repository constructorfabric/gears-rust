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
  - [Bootstrap Saga Retry Envelope](#bootstrap-saga-retry-envelope)
  - [Root-Tenant Finalization Saga](#root-tenant-finalization-saga)
- [4. States (CDSL)](#4-states-cdsl)
  - [Root Tenant Bootstrap Lifecycle](#root-tenant-bootstrap-lifecycle)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement Root Tenant Auto-Creation](#implement-root-tenant-auto-creation)
  - [Implement Root Tenant IdP Linking](#implement-root-tenant-idp-linking)
  - [Implement Bootstrap Idempotency](#implement-bootstrap-idempotency)
  - [Implement Bootstrap IdP-Retry Envelope](#implement-bootstrap-idp-retry-envelope)
  - [Implement Bootstrap Audit and Metrics Emission](#implement-bootstrap-audit-and-metrics-emission)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Deliberate Omissions](#7-deliberate-omissions)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-account-management-featstatus-platform-bootstrap`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-account-management-feature-platform-bootstrap`
## 1. Feature Context

### 1.1 Overview

AM automatically creates the initial root tenant on first platform start, links it to the configured IdP provider, and blocks the gear ready-signal until both succeed. Idempotent across restarts and platform upgrades; a stale `provisioning` row left by a prior failed attempt defers to the Provisioning Reaper rather than being retried in place.

### 1.2 Purpose

Implements PRD §5.1 Platform Bootstrap — the foundation FR group without which no tenant hierarchy can exist. The feature owns the one-time wiring moment at `AccountManagementGear` lifecycle entry: idempotency detection against the existing `tenants` table and a three-step saga whose retry envelope (`bootstrap.idp_retry_backoff_initial` doubling up to `bootstrap.idp_retry_backoff_max`, bounded by `bootstrap.idp_wait_timeout`) bounds `provision_tenant` re-attempts. `provision_tenant` is itself the readiness signal — there is no separate availability probe. The **overall saga is not atomic**: step 1 (insert the `provisioning` tenant row) and step 3 (flip `status → active` and add the closure self-row) are each their own short DB transaction, but step 2 — `IdpPluginClient::provision_tenant` — runs **outside** any DB transaction and is the compensating boundary. A clean step-2 failure deletes the `provisioning` row in a compensating TX and surfaces `CanonicalError::ServiceUnavailable` (HTTP 503); an ambiguous step-2 outcome leaves the row for the Provisioning Reaper. Only step 3's status flip + closure self-row insert commit atomically together. A stuck `provisioning` root observed at classify time (older than `2 × bootstrap.idp_wait_timeout`) is reaped synchronously in-band: bootstrap issues one `deprovision_tenant` + row-compensation pass and, on confirmed cleanup, restarts the saga from `no-root`; on any non-clean outcome it falls through to the `deferred_to_reaper` terminal.

**Requirements**: `cpt-cf-account-management-fr-root-tenant-creation`, `cpt-cf-account-management-fr-root-tenant-idp-link`, `cpt-cf-account-management-fr-bootstrap-idempotency`, `cpt-cf-account-management-fr-bootstrap-ordering`

**Principles**: `cpt-cf-account-management-principle-source-of-truth` (bootstrap establishes the root tenant as the canonical hierarchy anchor), `cpt-cf-account-management-principle-idp-agnostic` (bootstrap is the first invocation of the IdP pluggable contract).

> **Note on DECOMPOSITION alignment**: this FEATURE claims the two principles above because bootstrap is the feature that first instantiates each one; `DECOMPOSITION.md` §2.1 lists the same principles under `Design Principles Covered`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-account-management-actor-platform-admin` | Configures the independent AM-owned root contract (`root_tenant_type.gts_id`, `root_tenant_type.idp_provisioning`) and optional bootstrap parameters (`bootstrap.root_id`, `bootstrap.root_name`, `bootstrap.root_tenant_metadata`, and IdP retry/timeout values) before platform start; observes bootstrap outcome via audit + metrics. |
| `cpt-cf-account-management-actor-idp` | Receives `provision_tenant(IdpProvisionTenantRequest{ tenant_id=root_id, ... })` during the saga; returns an optional opaque `IdpProvisionResult::metadata` blob that AM persists into `tenant_idp_metadata` (one row per tenant, plugin-owned shape). |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §5.1 Platform Bootstrap
- **Design**: [DESIGN.md](../DESIGN.md) §3.2 `BootstrapService` + `AccountManagementGear`, §3.6 `seq-bootstrap`
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.1 Platform Bootstrap
- **Dependencies**: `cpt-cf-account-management-feature-errors-observability` (error taxonomy + bootstrap-lifecycle metric family + `actor=system` audit contract)

## 2. Actor Flows (CDSL)

Bootstrap is triggered by the `AccountManagementGear` lifecycle rather than an end-user request. The flow below traces the indirect actor path: Platform Administrator's deployment configuration drives ToolKit's `lifecycle(entry = ...)` invocation, which in turn drives `BootstrapService`.

### Platform Bootstrap Saga

- [ ] `p1` - **ID**: `cpt-cf-account-management-flow-platform-bootstrap-saga`

**Actor**: `cpt-cf-account-management-actor-platform-admin`

**Success Scenarios**:

- First platform start: AM stages the configured `root_tenant_type.gts_id` through the existing Types Registry client and the registry ready transition semantically validates it (no duplicated platform-root YAML seed is required); when bootstrap is enabled, the root tenant is created with that type, its IdP binding is established, status transitions `provisioning → active`, `tenant_closure` self-row is present, and the gear signals ready.
- Restart after prior success: idempotent detection finds the existing `active` root; saga is skipped; gear signals ready immediately.
- IdP briefly unavailable at start: wait-loop backs off and eventually proceeds when IdP reports available within the configured total timeout.

**Error Scenarios**:

- IdP never becomes available within total timeout: bootstrap fails with `CanonicalError::ServiceUnavailable` (HTTP 503); no `provisioning` row is left behind; gear does not signal ready.
- Root-type registration fails: Types Registry is unavailable, the identifier is already staged with conflicting content, semantic validation fails during the registry ready transition, or an existing root's durable type UUID differs. This is lifecycle-fatal regardless of `bootstrap.strict`; no new `tenants` row is written.
- Finalization fails after successful `provision_tenant`: `tenants` row remains in `provisioning`; Provisioning Reaper compensates on its next sweep; next bootstrap attempt recreates the root.
- Concurrent replicas race: the `ux_tenants_single_root` unique partial index prevents duplicate roots; the losing replica hits a constraint violation and falls through to the idempotency path on its next classification attempt.

**Steps**:

1. [ ] - `p1` - ToolKit invokes `AccountManagementGear.lifecycle(entry = ...)` — `inst-flow-bootstrap-lifecycle-entry`
2. [ ] - `p1` - Gear calls `BootstrapService.run(bootstrap_config)` before signalling gear ready - `inst-flow-bootstrap-invoke-service`
3. [ ] - `p1` - Increment `bootstrap.attempts` counter (per attempt, before any DB work) - `inst-flow-bootstrap-metric-attempt`
4. [ ] - `p1` - Query for existing root tenant via TenantService.find_root() - `inst-flow-bootstrap-detect-root`
5. [ ] - `p1` - Run idempotency classification via `algo-platform-bootstrap-idempotency-detection` over the result - `inst-flow-bootstrap-classify-idempotency`
6. [ ] - `p1` - **IF** classification = `active-root-exists` - `inst-flow-bootstrap-branch-active`
   1. [ ] - `p1` - Emit audit event `bootstrapSkipped` (camelCase wire form per `AuditEventKind::as_str` / Serde `rename_all = "camelCase"`) with `actor=system` - `inst-flow-bootstrap-audit-skipped`
   2. [ ] - `p1` - Emit `bootstrap.outcome` counter labeled `classification=skipped` - `inst-flow-bootstrap-metric-outcome-skipped`
   3. [ ] - `p1` - **RETURN** Bootstrap skipped (idempotent) - `inst-flow-bootstrap-return-skip`
7. [ ] - `p1` - **IF** classification = `provisioning-root-observed` (the root row is in `provisioning` status). Distinguishing **stuck** (winner crashed mid-saga) from **in-progress-elsewhere** (another replica is currently running steps 1–3) requires an age check against `tenants.created_at`: rows older than `bootstrap.idp_wait_timeout * 2` enter the in-band stuck-row compensation path described below; younger rows are classified as `in-progress-elsewhere` and the losing replica returns immediately without touching the row, which keeps the concurrent-replicas race narrative consistent. - `inst-flow-bootstrap-branch-stuck`
   1. [ ] - `p1` - **IF** observed `created_at < now - 2 * bootstrap.idp_wait_timeout` (stuck): attempt one synchronous `IdpPluginClient::deprovision_tenant` + `compensate_provisioning` pass bounded by the bootstrap deadline; on confirmed cleanup re-enter classification (which will see `no-root` and run the standard fresh-saga path); on any non-clean outcome emit audit event `bootstrapDeferredToReaper` with `actor=system`; emit `bootstrap.outcome` counter labeled `classification=deferred_to_reaper`; **RETURN** Bootstrap not complete, await reaper compensation - `inst-flow-bootstrap-audit-deferred`
   2. [ ] - `p1` - **ELSE** (in-progress-elsewhere): emit `bootstrap.outcome` counter labeled `classification=in_progress_elsewhere`; **RETURN** Bootstrap not complete, peer replica is finalizing — no compensation needed - `inst-flow-bootstrap-return-stuck`
8. [ ] - `p1` - **IF** classification = `invariant-violation` (suspended or deleted status on root row — illegal pre-existing state) - `inst-flow-bootstrap-branch-invariant`
   1. [ ] - `p1` - Emit audit event `bootstrapInvariantViolation` with `actor=system` + observed-status detail - `inst-flow-bootstrap-audit-invariant`
   2. [ ] - `p1` - Emit `bootstrap.outcome` counter labeled `classification=invariant_violation` (metric label values are snake_case per the workspace convention; the camelCase `bootstrapInvariantViolation` audit event name above follows the Serde `rename_all = "camelCase"` wire format pinned by `AuditEventKind::as_str` and is a different surface) - `inst-flow-bootstrap-metric-outcome-invariant`
   3. [ ] - `p1` - **RETURN** lifecycle-fatal `RootBindingMismatch` (HTTP 500) per `feature-errors-observability`; unlike ordinary bootstrap failures, `bootstrap.strict = false` cannot suppress an invalid root lifecycle. The `bootstrapInvariantViolation` audit event name and the `classification=invariant_violation` metric label above remain internal-only labels, not part of the public Problem envelope. Root is in an illegal state — manual intervention required. - `inst-flow-bootstrap-return-invariant`
9. [ ] - `p1` - **ELSE** (classification = `no-root`; proceed to create root) - `inst-flow-bootstrap-branch-create`
   1. [ ] - `p1` - Execute `algo-platform-bootstrap-finalization-saga` against `TenantService.create_root_tenant(bootstrap_config)`; the saga itself bounds its `provision_tenant` retry envelope by `bootstrap.idp_wait_timeout` (each `clean_failure` reschedules after `compensate_provisioning`, doubled backoff capped at `idp_retry_backoff_max`); on retry exhaustion the saga surfaces `clean_failure` mapped to `CanonicalError::ServiceUnavailable` (HTTP 503) - `inst-flow-bootstrap-run-finalization`
   2. [ ] - `p1` - **IF** finalization succeeded - `inst-flow-bootstrap-finalize-success`
      1. [ ] - `p1` - Emit audit event `bootstrapCompleted` with `actor=system` - `inst-flow-bootstrap-audit-completed`
      2. [ ] - `p1` - Emit `bootstrap.outcome` counter labeled `classification=completed` - `inst-flow-bootstrap-metric-outcome-completed`
      3. [ ] - `p1` - **RETURN** Bootstrap complete - `inst-flow-bootstrap-return-ok`
   3. [ ] - `p1` - **ELSE** finalization returned `clean_failure` or `ambiguous_failure` - `inst-flow-bootstrap-finalize-fail`
      1. [ ] - `p1` - Emit audit event `bootstrapFinalizationFailed` with `actor=system` + failure reason and failure class - `inst-flow-bootstrap-audit-failed`
      2. [ ] - `p1` - Emit `bootstrap.outcome` counter labeled `classification=clean_failure` or `classification=ambiguous_failure` - `inst-flow-bootstrap-metric-outcome-failed`
      3. [ ] - `p1` - **IF** finalization returned `clean_failure` - `inst-flow-bootstrap-clean-failure`
         1. [ ] - `p1` - **RETURN** Bootstrap not complete; no root state remains and a later retry may run the saga again - `inst-flow-bootstrap-return-clean-fail`
      4. [ ] - `p1` - **ELSE** finalization returned `ambiguous_failure` - `inst-flow-bootstrap-ambiguous-failure`
         1. [ ] - `p1` - **RETURN** Bootstrap not complete; root remains in `provisioning` for reaper compensation - `inst-flow-bootstrap-return-ambiguous-fail`

## 3. Processes / Business Logic (CDSL)

### Idempotency Detection

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-platform-bootstrap-idempotency-detection`

**Input**: Result of `TenantService.find_root()` — zero or one tenant record (id, status)

**Output**: Classification enum — `active-root-exists`, `provisioning-root-stuck`, `no-root`, or `invariant-violation`

**Steps**:

1. [ ] - `p1` - Parse the query result into `{ row_count, first_status }` - `inst-algo-idem-parse`
2. [ ] - `p1` - **IF** the row count is 0 - `inst-algo-idem-no-row`
   1. [ ] - `p1` - **RETURN** `no-root` - `inst-algo-idem-return-no-root`
3. [ ] - `p1` - **IF** the first status is 1 (active) - `inst-algo-idem-row-active`
   1. [ ] - `p1` - **RETURN** `active-root-exists` - `inst-algo-idem-return-active`
4. [ ] - `p1` - **IF** the first status is 0 (provisioning) - `inst-algo-idem-row-provisioning`
   1. [ ] - `p1` - **RETURN** `provisioning-root-stuck` - `inst-algo-idem-return-stuck`
5. [ ] - `p1` - **ELSE** unexpected status (suspended or deleted on a root row) - `inst-algo-idem-invariant-violation`
   1. [ ] - `p1` - **RETURN** `invariant-violation` (fail-fast — root cannot be suspended or deleted) - `inst-algo-idem-return-invariant`

### Bootstrap Saga Retry Envelope

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-platform-bootstrap-idp-wait-with-backoff`

**Input**: `bootstrap_config` containing `bootstrap.idp_retry_backoff_initial` (default 2s), `bootstrap.idp_retry_backoff_max` (default 30s), `bootstrap.idp_wait_timeout` (default 5min)

**Output**: Result — `success` (root activated), `idp_unavailable` (retry envelope exhausted on `clean_failure`), or `ambiguous` (provisioning row left for reaper)

**Steps**:

1. [ ] - `p1` - Initialize `current_backoff = idp_retry_backoff_initial`; record start timestamp for deadline calculation - `inst-algo-wait-init`
2. [ ] - `p1` - Run `algo-platform-bootstrap-finalization-saga` once - `inst-algo-wait-try-saga`
   1. [ ] - `p1` - **IF** saga returns `success` (root activated) - `inst-algo-wait-saga-ok`
      1. [ ] - `p1` - Emit metric `bootstrap.idp_wait.duration` with observed `elapsed` - `inst-algo-wait-metric-ok`
      2. [ ] - `p1` - **RETURN** `success` - `inst-algo-wait-return-ok`
3. [ ] - `p1` - **CATCH** saga result `clean_failure` (no IdP-side state retained; row already compensated) - `inst-algo-wait-catch`
   1. [ ] - `p1` - **IF** `elapsed >= bootstrap.idp_wait_timeout` - `inst-algo-wait-check-timeout`
      1. [ ] - `p1` - Emit metric `bootstrap.idp_wait.timeout` (counter) - `inst-algo-wait-metric-timeout`
      2. [ ] - `p1` - **RETURN** `idp_unavailable` - `inst-algo-wait-return-timeout`
   2. [ ] - `p1` - Sleep for `current_backoff` - `inst-algo-wait-sleep`
   3. [ ] - `p1` - Update elapsed from recorded start - `inst-algo-wait-update-elapsed`
   4. [ ] - `p1` - Double `current_backoff`, capping at `idp_retry_backoff_max` - `inst-algo-wait-grow-backoff`
   5. [ ] - `p1` - Retry the saga from step 2 (which re-enters classification — fresh `no-root` after `clean_failure` already compensated the row) - `inst-algo-wait-retry`

### Root-Tenant Finalization Saga

- [ ] `p1` - **ID**: `cpt-cf-account-management-algo-platform-bootstrap-finalization-saga`

**Input**: independent `root_tenant_type` contract plus `bootstrap_config` (`root_id`, `root_name`, `root_tenant_metadata`, and saga policy)

**Output**: Result — `success` (root tenant visible in `active` status with self-closure row), `clean_failure` (no AM or IdP root state retained; safe to retry), or `ambiguous_failure` (provisioning row persists, awaits reaper)

**Steps**:

> This algorithm describes the saga at the `TenantService` abstraction level. `TenantService` owns the concrete DB operations (schema layout, column-level ORM calls, transaction boundaries); the algo specifies *which* service methods are invoked and in what order, not *how* they are implemented. See DESIGN §3.2 `TenantService` component + §3.6 `seq-bootstrap` for the authoritative DB-level contract.

1. [ ] - `p1` - Resolve the independently registered `root_tenant_type.gts_id` through the existing `TypesRegistryClient` using DESIGN §3.1 effective-trait resolution; this bootstrap-owned root preflight does not call downstream barrier features because bootstrap is earlier in the feature DAG - `inst-algo-saga-type-check`
2. [ ] - `p1` - **IF** GTS is unavailable, times out, or cannot resolve effective traits - `inst-algo-saga-type-gts-unavailable`
   1. [ ] - `p1` - **RETURN** `clean_failure` with the delegated `service_unavailable` classification from `errors-observability`; no DB state persisted and no IdP call issued - `inst-algo-saga-return-gts-unavailable`
3. [ ] - `p1` - **IF** the configured root type is not a registered two-segment tenant type directly under `gts.cf.core.am.tenant_type.v1~` - `inst-algo-saga-type-invalid-branch`
   1. [ ] - `p1` - **RETURN** `clean_failure` and surface the failure through the `errors-observability` envelope as `CanonicalError::InvalidArgument` (HTTP 400) carrying the exact `reason=INVALID_TENANT_TYPE` token on the field-violation entry per DESIGN §3.8 — the `reason` token is the stable wire discriminator clients switch on and **MUST NOT** be remapped to a broader category token; no DB state persisted - `inst-algo-saga-return-invalid-type`
4. [ ] - `p1` - **ELSE IF** the effective `allowed_parent_types` trait is not exactly `[]` after default resolution - `inst-algo-saga-type-not-root-branch`
   1. [ ] - `p1` - **RETURN** `clean_failure` and surface the failure through the `errors-observability` envelope as `CanonicalError::FailedPrecondition` (HTTP 400) carrying the exact `reason=TYPE_NOT_ALLOWED` token on the precondition-violation entry per DESIGN §3.8 — the `reason` token is the stable wire discriminator clients switch on and **MUST NOT** be remapped to a broader category token; no DB state persisted - `inst-algo-saga-return-not-root-type`
5. [ ] - `p1` - **TRY** saga step 1 (short TX, TenantService-owned) - `inst-algo-saga-step-1`
   1. [ ] - `p1` - TenantService: insert root tenant row in `provisioning` status (no parent, depth 0, not self-managed, resolved type uuid) and commit the transaction - `inst-algo-saga-insert-provisioning`
6. [ ] - `p1` - **CATCH** saga step 1 error - `inst-algo-saga-step-1-catch`
   1. [ ] - `p1` - **RETURN** `clean_failure` (no row persisted; no cleanup needed) - `inst-algo-saga-return-step-1-fail`
7. [ ] - `p1` - **TRY** saga step 2 (IdP call, no open TX) - `inst-algo-saga-step-2`
   1. [ ] - `p1` - IdP: `provision_tenant(IdpProvisionTenantRequest{ tenant_id=root_id, tenant_name=root_name, tenant_type=root_tenant_type.gts_id, parent_id=None, tenant_metadata=root_tenant_metadata })` - `inst-algo-saga-idp-call`
   2. [ ] - `p1` - Receive `IdpProvisionResult { metadata: Option<opaque JSON blob> }` — AM does not inspect or validate the blob - `inst-algo-saga-receive-result`
8. [ ] - `p1` - **CATCH** saga step 2 error - `inst-algo-saga-step-2-catch`
   1. [ ] - `p1` - **IF** the provider result proves no IdP-side root state was retained - `inst-algo-saga-step-2-clean-branch`
      1. [ ] - `p1` - TenantService: delete the `provisioning` root row in a short compensating transaction; **RETURN** `clean_failure` mapped to `CanonicalError::ServiceUnavailable` (HTTP 503) with safe retry semantics - `inst-algo-saga-return-step-2-clean`
   2. [ ] - `p1` - **ELSE** the external outcome is ambiguous or may already be retained by the IdP - `inst-algo-saga-step-2-ambiguous-branch`
      1. [ ] - `p1` - **RETURN** `ambiguous_failure` (provisioning row left for reaper to compensate per seq-bootstrap; caller must reconcile before blind retry) - `inst-algo-saga-return-step-2-ambiguous`
9. [ ] - `p1` - **TRY** saga step 3 (finalize, short TX, TenantService-owned) - `inst-algo-saga-step-3`
   1. [ ] - `p1` - TenantService: upsert the opaque `IdpProvisionResult::metadata` blob (if `Some`) into `tenant_idp_metadata` keyed by `tenant_id`, transition root status to `active`, and insert the root's self-row in `tenant_closure` (`ancestor = descendant = root_id`, barrier = 0, descendant_status = active) — all in a single transaction - `inst-algo-saga-finalize`
   2. [ ] - `p1` - **RETURN** `success` - `inst-algo-saga-return-success`
10. [ ] - `p1` - **CATCH** saga step 3 error (e.g. DB unavailable, constraint violation) - `inst-algo-saga-step-3-catch`
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

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-platform-bootstrap-root-creation`

The system **MUST** create exactly one root tenant row (`parent_id IS NULL`) during the first successful bootstrap, finalize it to `active` status, and write the corresponding self-row in `tenant_closure`. Bootstrap **MUST NOT** expose a root tenant in `active` status until the three-step saga has fully committed.

**Implements**:

- `cpt-cf-account-management-flow-platform-bootstrap-saga`
- `cpt-cf-account-management-algo-platform-bootstrap-finalization-saga`
- `cpt-cf-account-management-state-platform-bootstrap-root-tenant-status`

**Touches**:

- DB: `tenants`, `tenant_closure`, `tenant_idp_metadata`
- Entities: `Tenant`, `TenantClosure`

### Implement Root Tenant IdP Linking

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-platform-bootstrap-idp-linking`

The system **MUST** invoke the IdP provider's `provision_tenant(IdpProvisionTenantRequest{ tenant_id=root_id, tenant_name=root_name, tenant_type=root_tenant_type.gts_id, parent_id=None, tenant_metadata=root_tenant_metadata })` exactly once during a successful bootstrap and **MUST** upsert the opaque `IdpProvisionResult::metadata` blob returned by the plugin (if any) into `tenant_idp_metadata` keyed by `tenant_id` in the finalization transaction. Bootstrap **MUST NOT** semantically interpret, namespace, or validate `root_tenant_metadata` or the returned blob — both sides are forwarded as-is between the deployer config and the plugin (the plugin owns the JSON shape end-to-end). When the plugin returns no metadata, bootstrap **MUST NOT** write a `tenant_idp_metadata` row.

**Implements**:

- `cpt-cf-account-management-algo-platform-bootstrap-finalization-saga`

**Constraints**: `cpt-cf-account-management-principle-idp-agnostic`

**Touches**:

- DB: `tenant_idp_metadata`
- Entities: `Tenant`

### Implement Bootstrap Idempotency

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-platform-bootstrap-idempotency`

The system **MUST** detect an existing active root tenant on platform restart or upgrade and complete bootstrap as a no-op. When a `provisioning` root row is observed (stuck from a prior failed attempt), bootstrap **MUST** defer to the Provisioning Reaper and **MUST NOT** create a second root or re-run the saga against the stale row.

**Implements**:

- `cpt-cf-account-management-flow-platform-bootstrap-saga`
- `cpt-cf-account-management-algo-platform-bootstrap-idempotency-detection`

**Touches**:

- DB: `tenants`
- Entities: `Tenant`

### Implement Bootstrap IdP-Retry Envelope

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-platform-bootstrap-idp-wait-ordering`

The system **MUST** bound the bootstrap saga's `provision_tenant` re-attempts by the configured `bootstrap.idp_retry_backoff_initial` (default 2s) doubling up to `bootstrap.idp_retry_backoff_max` (default 30s) within a total `bootstrap.idp_wait_timeout` budget (default 5min). Each `IdpProvisionFailure::CleanFailure` already compensates the `provisioning` row before the retry, so retry exhaustion **MUST** surface `CanonicalError::ServiceUnavailable` (HTTP 503) with no partial row left behind. There is no separate availability probe — `provision_tenant` is itself the readiness signal.

**Implements**:

- `cpt-cf-account-management-algo-platform-bootstrap-idp-wait-with-backoff`

**Touches**:

- External contract: IdP provider plugin (`provision_tenant`)
- Metrics: `bootstrap.idp_wait.duration`, `bootstrap.idp_wait.timeout`

### Implement Bootstrap Audit and Metrics Emission

- [x] `p1` - **ID**: `cpt-cf-account-management-dod-platform-bootstrap-audit-and-metrics`

The system **MUST** emit `actor=system` platform audit events at every terminal bootstrap outcome (`bootstrapCompleted`, `bootstrapSkipped`, `bootstrapDeferredToReaper`, `bootstrapIdpTimeout`, `bootstrapInvariantViolation`, `bootstrapFinalizationFailed` — camelCase wire form pinned by `AuditEventKind::as_str` and the Serde `rename_all = "camelCase"` derive) and **MUST** export the bootstrap-lifecycle metric family (attempt counter, IdP-wait duration histogram, IdP-wait timeout counter, outcome counter by terminal classification) through the gear's observability plumbing owned by the errors-observability feature.

**Implements**:

- `cpt-cf-account-management-flow-platform-bootstrap-saga`
- `cpt-cf-account-management-algo-platform-bootstrap-idp-wait-with-backoff`

**Constraints**: Metric names and audit schema are anchored by the `errors-observability` feature's catalog; this feature contributes entries but does not redefine the catalog.

**Touches**:

- Platform audit sink
- Metrics: `bootstrap.attempts`, `bootstrap.outcome{classification}`, `bootstrap.idp_wait.duration`, `bootstrap.idp_wait.timeout`

## 6. Acceptance Criteria

- [ ] First platform start: root tenant row exists with `status = 1` (active), `parent_id IS NULL`, `depth = 0`; `tenant_closure` contains exactly one row `(root_id, root_id, 0, 1)`; gear signals ready; audit sink has a `bootstrapCompleted actor=system` event.
- [ ] Second platform start (post-success): no new `tenants` row is created; no second `provision_tenant` call is issued; audit sink has a `bootstrapSkipped actor=system` event; gear signals ready.
- [ ] Start observing a **stale** `provisioning` root row (`created_at < now - 2 * bootstrap.idp_wait_timeout`, prior saga crashed mid-flight): bootstrap attempts one synchronous in-band `deprovision_tenant` + `compensate_provisioning` pass. On confirmed cleanup the saga restarts on `no-root` and activates a fresh root within the same `run()`. On any non-clean outcome the stale row is left in place and bootstrap logs the defer-to-reaper outcome; `bootstrap.outcome` carries `classification=deferred_to_reaper`; audit sink has a `bootstrapDeferredToReaper actor=system` event; after successful Provisioning Reaper compensation, a subsequent start recreates the root through the full saga.
- [ ] Start observing a **young** `provisioning` root row (within the `2 * bootstrap.idp_wait_timeout` window — another replica is currently running steps 1–3): no second root created; no audit event is emitted (the `bootstrapDeferredToReaper` path is not taken); `bootstrap.outcome` carries `classification=in_progress_elsewhere`; gear does not signal ready and the call returns immediately so the peer can finalize without interference.
- [ ] IdP unavailable for longer than `bootstrap.idp_wait_timeout` (every saga attempt returned `IdpProvisionFailure::CleanFailure`, each compensated by the saga before the next retry): bootstrap returns `CanonicalError::ServiceUnavailable` (HTTP 503); no `tenants` row is left in `provisioning`; `bootstrap.idp_wait.timeout` metric is incremented; gear does not signal ready.
- [ ] Concurrent replica starts on a fresh database: exactly one replica wins the insert race and creates the root. Each losing replica's insert attempt hits the `ux_tenants_single_root` unique constraint; the CATCH branch maps the unique-violation to `clean_failure` and returns immediately on the current attempt (no DB side effects, safe to retry). On the *next* bootstrap attempt — once the winning replica has finalized the root through `provisioning → active` — the loser's classification step finds the active root and returns `bootstrapSkipped`. No duplicate `tenants` or `tenant_closure` rows exist at any point.
- [ ] A registered `root_tenant_type.gts_id` that cannot be resolved during bootstrap: the bootstrap-owned root-type preflight returns `clean_failure` surfaced as `CanonicalError::InvalidArgument` (HTTP 400) carrying the canonical `reason = "INVALID_TENANT_TYPE"` token on the field-violation entry per DESIGN §3.8 — **before** saga step 1 begins; no `tenants` row is written; no IdP call is issued. A root type whose effective `allowed_parent_types` value is not `[]` fails the same way as `CanonicalError::FailedPrecondition` (HTTP 400) with `reason = "TYPE_NOT_ALLOWED"` on the precondition-violation entry; a GTS transport/timeout failure returns the delegated `service_unavailable` classification (`CanonicalError::ServiceUnavailable`, HTTP 503) with no DB side effects. The `reason` token is the stable wire discriminator clients switch on; AM does not surface AM-private `code=` strings.
- [ ] During `provision_tenant`, a provider failure that proves no IdP-side root state was retained deletes the `provisioning` row in a compensating transaction and returns `clean_failure` mapped to `CanonicalError::ServiceUnavailable` (HTTP 503); the next bootstrap retry may safely re-run the saga. A transport timeout or ambiguous provider result leaves the `provisioning` row for the reaper, returns `ambiguous_failure`, and does not invite blind automatic retry.
- [ ] Start observing a suspended or deleted root tenant row (illegal pre-existing state): bootstrap returns lifecycle-fatal `RootBindingMismatch` (HTTP 500) even when `bootstrap.strict = false`, and emits the `classification=invariant_violation` metric label on `bootstrap.outcome`; no second root is created; gear does not signal ready; audit sink has a `bootstrapInvariantViolation actor=system` event.

## 7. Deliberate Omissions

The following concerns are explicitly **not** addressed by this FEATURE. Each is recorded so reviewers can distinguish intentional exclusion (author considered and excluded with reasoning) from accidental omission.

- **UX / usability** — *Not applicable.* Bootstrap is a system-internal lifecycle operation triggered by ToolKit gear startup; it has no user-facing interface, no user input, and no interaction surface. Observability for operators (audit + metrics) is covered by §5.5 and delegated to `errors-observability`.
- **Regulatory compliance / data-subject rights** — *Not applicable.* Bootstrap creates no user data, collects no consent, and has no retention or data-subject-rights surface. The only data written is AM-internal structural rows (root tenant, closure self-row, optional provider metadata).
- **Types Registry durability** — *Deferred to #4627.* Root-type registration in this feature uses the existing process-local catalogue. It provides no durable state or coordination between replicas, and rereading after ready proves only local in-memory presence. Restart and cross-replica guarantees are covered by the Types Registry P0 SDK and consumer migration (T23–T26), not by this feature.
- **Data privacy (PII)** — *Not applicable.* `root_tenant_metadata` is an opaque deployment-configuration blob that AM forwards as-is to the IdP provider plugin without interpretation, and the optional `IdpProvisionResult::metadata` blob AM persists into `tenant_idp_metadata` is plugin-private state whose JSON shape is owned entirely by the plugin — AM neither introspects nor normalizes either side, which keeps bootstrap out of any PII-handling boundary.
- **Concrete metric names and audit-event schemas** — *Owned by `errors-observability`.* This FEATURE references the `bootstrap.*` metric family and `bootstrap.*` audit-event names by stable label but does not define their carrier schema, cardinality limits, or retention; those contracts live in the errors-observability FEATURE's metric catalog and audit-event registry.
- **Conforming IdP plugin implementations** — *Out of scope.* The pluggable IdP contract is referenced via `provision_tenant` but the individual provider plugins (Keycloak, custom IdPs, etc.) are separate crates owned by the `idp-user-operations-contract` feature; this FEATURE tests only that the contract is invoked correctly and the IdpProvisionResult is persisted.
