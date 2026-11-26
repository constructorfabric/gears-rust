<!-- Created: 2026-08-24 by Constructor Tech -->

# Decomposition: Quota Enforcement

**Overall implementation status:**

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-status-implementation`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Gear Foundation, Storage & Coordination (HIGH)](#21-gear-foundation-storage--coordination-high)
  - [2.2 Metric Types & Subject Admission (HIGH)](#22-metric-types--subject-admission-high)
  - [2.3 Quota Lifecycle & Metadata (HIGH)](#23-quota-lifecycle--metadata-high)
  - [2.4 Resolution Policy & Engine (HIGH)](#24-resolution-policy--engine-high)
  - [2.5 Consumption Operations & Idempotency (HIGH)](#25-consumption-operations--idempotency-high)
  - [2.6 Lease Operations (HIGH)](#26-lease-operations-high)
  - [2.7 Batch Debit (MEDIUM)](#27-batch-debit-medium)
  - [2.8 Quota Snapshot Reads (MEDIUM)](#28-quota-snapshot-reads-medium)
  - [2.9 Notification Outbox & Dispatch (MEDIUM)](#29-notification-outbox--dispatch-medium)
  - [2.10 Bulk Quota CRUD (MEDIUM)](#210-bulk-quota-crud-medium)
  - [2.11 Rate Quotas (LOW)](#211-rate-quotas-low)
  - [2.12 Quota Resolver Plugin (HIGH)](#212-quota-resolver-plugin-high)
  - [2.13 Deliberate Omissions & Shared Elements](#213-deliberate-omissions--shared-elements)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

The DESIGN is decomposed into twelve features along its plugin seams and evaluation pipeline. The strategy is
dependency-ordered vertical slices: the foundation feature stands up the gear skeleton, the storage plugin contract,
the coordination adapter over the platform `cluster` gear, authorization, and tenant isolation; the
projection-contracts feature (Metric Types & Subject Admission) adds the catalogue of metric types that requests and
Quota and Policy writes are checked against. A **metric type** is the GTS type that the owner of a metric registers in
`types-registry` (ADR-0013). Policy/engine, lifecycle, and the operation features then build the evaluation pipeline
in the order the `EvaluationOrchestrator` executes it. Read surfaces, notifications, and the deferred P2/P3
capabilities follow; the quota resolver plugin, which exposes the finished operations to the quota resolver, closes the list. Every P1 functional requirement, NFR,
design principle, constraint, component, sequence, and the database schema is covered by exactly the features that
implement it; deferred requirements (`p2`/`p3`) are carried by dedicated features so no P1 feature depends on deferred
work. Each feature is a single implementation phase; sub-decomposition below the feature level happens in the
per-feature FEATURE documents, not here.

## 2. Entries

### 2.1 Gear Foundation, Storage & Coordination (HIGH)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-foundation`

- **Feature**: [features/foundation.md](features/foundation.md)

- **Purpose**: Stand up the `quota-enforcement` gear and SDK crates, the `QuotaEnforcementStoragePluginV1` contract
  with its `toolkit-db` reference plugin, the coordination adapter over the platform `cluster` gear's leader election, gear
  bootstrap (schema check, config-table defaults, and the bootstrap hook later features extend),
  two-phase PDP authorization with the trusted admission mode of the quota resolver plugin path, tenant isolation, and
  the gear-specific telemetry conventions. Every later feature builds on these seams.

- **Depends On**: None

- **Scope**:
  - `quota-enforcement` and `quota-enforcement-sdk` crate skeletons registered in the workspace
  - `QuotaEnforcementStoragePluginV1` trait, closed `StorageError` enum, invariants I1–I15, `toolkit-db` plugin
  - `cluster-sdk` dependency (no `deps = [cluster]` edge), typed `quota-enforcement` cluster profile, and the
    coordination adapter (one leader election per sweeper scope, linearizable requirement validated at startup)
  - Gateway REST registration into the platform `api-gateway`, DTO validation shell, phase-1 PDP admission via
    `PolicyEnforcer` (fail-closed, no QE-side decision cache), phase-2 `AccessScope` pass-through to `SecureConn`
  - Trusted admission mode for the quota resolver plugin path: no PDP call, a tenant-only `AccessScope`, policy
    management refused (ADR-0009)
  - Tenant isolation filters at gateway and storage layers
  - Bootstrap: schema-version check, default config-table rows, fail-fast posture, and the extension hook later
    features add their bootstrap steps to
  - Telemetry emission conventions with bounded label cardinality

- **Out of scope**:
  - Metric types and the catalogue that holds them (feature `projection-contracts`)
  - Any evaluation, lease, or notification behavior (later features)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-pluggable-storage`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-authorization`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-tenant-isolation`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-telemetry`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-authentication`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-authorization`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-tenant-isolation-integrity`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-principle-fail-closed`
  - [ ] `p1` - `cpt-cf-quota-enforcement-principle-storage-pluggable`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-constraint-toolkit`
  - [ ] `p1` - `cpt-cf-quota-enforcement-constraint-single-storage-plugin`
  - [ ] `p1` - `cpt-cf-quota-enforcement-constraint-security-context`
  - [ ] `p1` - `cpt-cf-quota-enforcement-constraint-no-business-logic`
  - [ ] `p1` - `cpt-cf-quota-enforcement-constraint-bounded-cardinality`

- **Domain Model Entities**:
  - `MutationResult`
  - configuration rows (`contention_timeout_config`, `lease_capacity_config`, `idempotency_retention_config`)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-gateway`
  - [ ] `p1` - `cpt-cf-quota-enforcement-component-storage-plugin`
  - [ ] `p1` - `cpt-cf-quota-enforcement-component-coordination-plugin`

- **API**:
  - REST route registration under `/quota-enforcement/v1/...` (routes land with their owning features)
  - `QuotaEnforcementStoragePluginV1` plugin trait in the SDK crate; cluster leader election consumed through
    `cluster-sdk`

- **Sequences**:

  - None (cross-cutting; every later sequence exercises this foundation)

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.2 Metric Types & Subject Admission (HIGH)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-projection-contracts`

- **Feature**: [features/projection-contracts.md](features/projection-contracts.md)

- **Purpose**: Read metric types from `types-registry` by their ids into `MetricCatalog`, the process-local catalogue
  with one entry per metric type that QE has read (ADR-0012), validate the metric object `{type, id?, metadata}` of
  every evaluation request at Gateway ingress against the schema of its metric type (ADR-0013), and check each named
  quota subject against the subject types the metric admits (ADR-0008). The whole evaluation pipeline trusts this
  validation.

- **Depends On**: `cpt-cf-quota-enforcement-feature-foundation`

- **Scope**:
  - `MetricCatalog` reads: at start for the metric types of the active Quotas and Policies; once, with a time limit,
    when a request or a Quota or Policy write names a metric type the catalogue does not hold; and on a bounded
    schedule, which rebuilds changed entries and removes the entry of a deleted metric type
  - Checks of an entry: the type is a valid metric type (not abstract, derived from the metric base of the quota
    resolver SDK, an own version of major 1 or higher, valid traits `kind`, `admitted_subjects`, `enforcement`); an
    entry that fails a check is unavailable (its requests fail with `FailedPrecondition`) and bootstrap does not
    fail; an entry is built from the registry content only, and stored Quotas and Policies are not checked
  - A metric type that is not registered: `InvalidArgument`, remembered for a short time; a registry read that fails
    or reaches its time limit: `ServiceUnavailable`
  - Admission checks on Quota and Policy writes: a served entry, and a subject type that the metric admits
  - Ingress validation: the metric object against the resolved schema of its metric type, at least one `{type, id}`
    subject, no two subjects of one type, a tenant subject naming the request's tenant, and admitted subject types
  - The `enforcement` trait of the metric type (`quota_gated` or `direct`) as the source of the metric's
    classification
  - Validation telemetry counters

- **Out of scope**:
  - Quota records themselves (feature `quota-lifecycle`)
  - Engine consumption of the metric's metadata (feature `resolution-policy-engine`)
  - The trusted admission mode (feature `foundation`)
  - Registration of GTS types: the quota resolver SDK owns the subject base, the tenant subject type, and the metric
    base; the owner of a metric registers its metric type; QE registers none
  - Moving Quotas or counters to another major version of a metric type, which is another metric (ADR-0013)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-projection-contracts`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-contract-validation`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-subject-type-registry`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-subject-resolution`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-principle-declarative-projection-contracts`
  - [ ] `p1` - `cpt-cf-quota-enforcement-principle-pdp-authorized-attribution`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-constraint-types-registry-delegation`

- **Domain Model Entities**:
  - `MetricCatalog`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-gateway`
  - [ ] `p1` - `cpt-cf-quota-enforcement-component-evaluation-orchestrator`

- **API**:
  - Consumer request fields (`tenant_id`, `subjects: [{type, id}]`, `metric: {type, id?, metadata}`) on debit, lease
    acquisition, preview, and every batch item
  - No registration endpoint: owners register metric types in `types-registry`

- **Sequences**:

  - None (ingress validation is a step inside every evaluation sequence)

- **Data**:

  - None (metric types are registry-resident; no QE-side table)

### 2.3 Quota Lifecycle & Metadata (HIGH)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-quota-lifecycle`

- **Feature**: [features/quota-lifecycle.md](features/quota-lifecycle.md)

- **Purpose**: Implement the Quota entity and its lifecycle — create, update, deactivate, read — with metric
  validation against the metric type, cap and validity-window semantics, size-capped metadata of conditions checked
  against the metric type when it is written, the closed `enforcement_mode` and `source` enums, rate-type rejection, and
  the deactivation cascade that resolves active leases atomically.

- **Depends On**: `cpt-cf-quota-enforcement-feature-foundation`,
  `cpt-cf-quota-enforcement-feature-projection-contracts`

- **Scope**:
  - `QuotaManagementService` with transactional CRUD via the storage plugin
  - Metric validation through the `MetricCatalog` entry of the Quota's metric type: the type is registered, its entry
    is served, and it admits the Quota's subject type; fail-closed
  - Cap semantics (`CAP_MUST_BE_NON_NEGATIVE`, `cap = 0`, `cap = null`, `CAP_BELOW_CONSUMED` at commit time)
  - Quota metadata: a JSON object of at most 4 KB of conditions over the request's metadata; on create and on a
    change of the metadata, QE checks them against the metric type, and a violation is `QUOTA_METADATA_REJECTED`
    with the list of what is wrong; stored metadata is never checked again
  - Validity-window storage and `currently_within_window` computation
  - Deactivation cascade marking leases resolved-by-deactivation
  - `rate` quota-type rejection with canonical `Unimplemented`

- **Out of scope**:
  - Counter mutation of any kind (features `consumption-operations`, `lease-operations`)
  - Bulk Quota CRUD (feature `bulk-quota-crud`, P2)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-quota-lifecycle`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-quota-metadata`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-metric-identity-validation`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-enforcement-mode`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-quota-type-rate-rejection`

- **Design Principles Covered**:

  - None (applies the principles of the foundation and projection-contracts features; introduces none of its own)

- **Design Constraints Covered**:

  - None (constraints are owned by the features that introduce them)

- **Domain Model Entities**:
  - `Quota`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-quota-management-service`

- **API**:
  - POST /quota-enforcement/v1/quotas
  - GET /quota-enforcement/v1/quotas/{id}
  - PATCH /quota-enforcement/v1/quotas/{id}
  - POST /quota-enforcement/v1/quotas/{id}/deactivate
  - GET /quota-enforcement/v1/quotas
  - `QuotaManagerApi` Quota methods (`create_quota`, `update_quota`, `deactivate_quota`, `read_quotas`)

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-quota-create`
  - `cpt-cf-quota-enforcement-seq-quota-deactivate-cascade`

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.4 Resolution Policy & Engine (HIGH)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-resolution-policy-engine`

- **Feature**: [features/resolution-policy-engine.md](features/resolution-policy-engine.md)

- **Purpose**: Implement the Quota Resolution Policy entity with immutable versioning and rollback, the
  `QuotaResolutionEngineV1` plugin contract with the `most-restrictive-wins` and `cel` built-ins, multi-Quota
  arbitration with strict Debit-Plan invariants, cascade and attribute-based selection expressiveness, and the
  hard-cap denial contract.

- **Depends On**: `cpt-cf-quota-enforcement-feature-foundation`,
  `cpt-cf-quota-enforcement-feature-projection-contracts`

- **Scope**:
  - `PolicyService` with scope precedence (per-metric over `global`) and version lifecycle
    (`active`/`superseded`/`rolled_back`/`deleted`), optimistic `if_match_version` concurrency
  - `EngineRegistry` with fail-fast bootstrap registration; seeding of the `global` Policy
    (`most-restrictive-wins`, version 1) as this feature's bootstrap extension, so no active Policy ever references an
    unregistered Engine
  - Engine contract: `id`/`validate_config`/`evaluate`, `ValidatedConfig` cache keyed by
    `(policy_id, policy_version)`
  - Debit Plan as a list of chains (ADR-0010) and its invariant enforcement at the Engine boundary with violation
    telemetry
  - `most-restrictive-wins` binding-Quota selection over the `Subject` and `Tenant` tiers and validity-window
    prefilter; sandboxed cost-bounded `cel`
  - Check of a `cel` Policy against the schema of its metric type, which describes `request`, the metadata of the
    request's metric
  - No check of existing Quotas when a Policy is created, changed, or replaced; the preview operation is the
    operator's tool for that
  - Operator-only Policy surface (`QuotaOperatorApi`)

- **Out of scope**:
  - Applying Debit Plans to counters (feature `consumption-operations`)
  - Additional Engine languages (P2-or-later per PRD §13)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-quota-resolution-policy`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-quota-resolution-policy-versioning`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-quota-resolution-engine`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-multi-quota-evaluation`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-quota-cascade`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-attribute-based-quota-selection`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-hard-quota-reject`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-principle-engine-pluggable`
  - [ ] `p1` - `cpt-cf-quota-enforcement-principle-strict-engine-boundary`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-constraint-in-process-engine-registration`

- **Domain Model Entities**:
  - `QuotaResolutionPolicy`
  - `QuotaResolutionPolicyVersion`
  - `EvaluationContext`
  - `Decision`
  - `QuotaDebitPlan`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-engine-registry`
  - [ ] `p1` - `cpt-cf-quota-enforcement-component-policy-service`

- **API**:
  - POST /quota-enforcement/v1/policies
  - GET /quota-enforcement/v1/policies/{id}
  - GET /quota-enforcement/v1/policies/{id}/versions
  - PATCH /quota-enforcement/v1/policies/{id}
  - POST /quota-enforcement/v1/policies/{id}/rollback
  - DELETE /quota-enforcement/v1/policies/{id}
  - `QuotaOperatorApi` Policy methods; `QuotaResolutionEngineV1` plugin trait

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-policy-version-update`

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.5 Consumption Operations & Idempotency (HIGH)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-consumption-operations`

- **Feature**: [features/consumption-operations.md](features/consumption-operations.md)

- **Purpose**: Implement the `EvaluationOrchestrator` pipeline and the single-shot counter operations — debit,
  credit, return, rollback, and read-only preview — over allocation and consumption counters, the charge ledger that
  makes part of a charge returnable (ADR-0011), with calendar-aligned UTC
  periods, deterministic rollover and settlement semantics, and the idempotency scope `(tenant, operation, key)` that
  makes every write replay-safe.

- **Depends On**: `cpt-cf-quota-enforcement-feature-quota-lifecycle`,
  `cpt-cf-quota-enforcement-feature-resolution-policy-engine`

- **Scope**:
  - `QuotaEnforcementService` entry points and the canonical orchestrator pipeline (resolution → idempotency lookup →
    locked read → Policy lookup → Engine → invariant check → mutation → persist → outbox → commit)
  - Allocation and consumption counter shapes with lazy period-row materialization and threshold-marker reset
  - Debit applying Engine Debit Plans atomically; credit against a named Quota; preview with no persisted state
  - Charge ledger written by every allowed debit and lease commit; unwinding of a chain plan; return of part of a
    charge, and rollback as the return of its remainder; deletion of charges that can no longer be returned
  - `INVALID_AMOUNT` fail-fast ordering; `IDEMPOTENCY_PAYLOAD_MISMATCH`; retention via `RetentionSweeper`
  - Period rollover event ordering and the calendar-keyed credit closure

- **Out of scope**:
  - Two-phase holds (feature `lease-operations`)
  - Multi-item envelopes (feature `batch-debit`)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-debit`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-credit`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-return`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-rollback`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-evaluate-preview`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-idempotency`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-period-semantics`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-period-rollover`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-quota-type-allocation`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-quota-type-consumption`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-evaluation-latency`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-throughput`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-subject-scale`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-quota-density`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-availability`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-fault-tolerance`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-idempotency-guarantee`

- **Design Principles Covered**:

  - None (executes the strict-engine-boundary and fail-closed principles owned by earlier features)

- **Design Constraints Covered**:

  - None (constraints are owned by the features that introduce them)

- **Domain Model Entities**:
  - `Counter` (allocation)
  - `Counter` (consumption)
  - `IdempotencyRecord`
  - `OperationLog`
  - `Charge`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-quota-enforcement-service`
  - [ ] `p1` - `cpt-cf-quota-enforcement-component-evaluation-orchestrator`
  - [ ] `p1` - `cpt-cf-quota-enforcement-component-idempotency-cache`
  - [ ] `p1` - `cpt-cf-quota-enforcement-component-retention-sweeper`

- **API**:
  - POST /quota-enforcement/v1/operations/debit
  - POST /quota-enforcement/v1/operations/credit
  - POST /quota-enforcement/v1/operations/rollback
  - POST /quota-enforcement/v1/operations/preview
  - `QuotaEnforcementApi` (`debit`, `rollback`, `evaluate_preview`)
  - `QuotaManagerApi` (`credit`)
  - Return: reached only through the quota resolver plugin (feature `resolver-plugin`)

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-debit`
  - `cpt-cf-quota-enforcement-seq-credit`
  - `cpt-cf-quota-enforcement-seq-rollback`
  - `cpt-cf-quota-enforcement-seq-evaluate-preview`
  - `cpt-cf-quota-enforcement-seq-idempotency-replay`
  - `cpt-cf-quota-enforcement-seq-period-rollover`

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.6 Lease Operations (HIGH)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-lease-operations`

- **Feature**: [features/lease-operations.md](features/lease-operations.md)

- **Purpose**: Implement the two-phase lease protocol — atomic multi-Quota acquisition, commit that unwinds the unused
  part from the holds and writes a charge, release, and TTL auto-release — under the lazy-expiry semantic that keeps
  correctness independent of sweeper liveness, with acquisition-period attribution, contention timeouts, and the
  per-`(tenant, metric)` active-lease cap.

- **Depends On**: `cpt-cf-quota-enforcement-feature-consumption-operations`

- **Scope**:
  - `LeaseManager` state machine (`Active` → `Committed`/`Released`/`AutoReleased`/`ResolvedByDeactivation`)
  - Atomic multi-Quota hold acquisition in lexicographic `quota_id` order; TTL bounds without clamping
  - Cross-period and cross-validity commit attribution to the acquisition period
  - Lazy semantic release on every read/write path; `LeaseSweeper` physical reclamation under the `lease-sweeper`
    cluster election
  - Contention timeout and active-lease-cap enforcement with their telemetry

- **Out of scope**:
  - Lease-resolution behavior of Quota deactivation (owned by feature `quota-lifecycle`)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-lease-acquire`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-lease-commit`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-lease-release`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-lease-timeout`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-recovery`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-principle-lazy-expiry`

- **Design Constraints Covered**:

  - None (constraints are owned by the features that introduce them)

- **Domain Model Entities**:
  - `Lease`
  - `LeaseHold`
  - `LeaseCapacityCounter`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-lease-manager`
  - [ ] `p1` - `cpt-cf-quota-enforcement-component-lease-sweeper`

- **API**:
  - POST /quota-enforcement/v1/leases
  - POST /quota-enforcement/v1/leases/{token}/commit
  - POST /quota-enforcement/v1/leases/{token}/release
  - `QuotaEnforcementApi` (`acquire_lease`, `commit_lease`, `release_lease`)

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-lease-acquire`
  - `cpt-cf-quota-enforcement-seq-lease-commit`
  - `cpt-cf-quota-enforcement-seq-lease-release`
  - `cpt-cf-quota-enforcement-seq-lease-auto-release`

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.7 Batch Debit (MEDIUM)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-batch-debit`

- **Feature**: [features/batch-debit.md](features/batch-debit.md)

- **Purpose**: Implement the multi-item debit envelope for single logical operations that consume several metrics
  at once: atomic all-or-nothing evaluation where each item observes the running batch state, envelope idempotency,
  the batch-level evaluation timeout, and P1 rejection of the deferred `independent` mode.

- **Depends On**: `cpt-cf-quota-enforcement-feature-consumption-operations`

- **Scope**:
  - `apply_batch_debit` storage primitive, the envelope idempotency key, and each item as a debit under its own key
  - `mode = atomic` semantics with per-item Decisions reported in submission order
  - Batch-level timeout superseding per-Policy timeouts; maximum batch size enforcement
  - `mode = independent` rejected with `NOT_YET_IMPLEMENTED`

- **Out of scope**:
  - Partial-success `independent` mode (P2 per PRD §5.7)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-batch-debit`

- **Design Principles Covered**:

  - None (reuses the pipeline principles owned by earlier features)

- **Design Constraints Covered**:

  - None (constraints are owned by the features that introduce them)

- **Domain Model Entities**:
  - `BatchItem`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-evaluation-orchestrator`

- **API**:
  - POST /quota-enforcement/v1/operations/batch-debit
  - `QuotaEnforcementApi` (`batch_debit`)

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-batch-debit`

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.8 Quota Snapshot Reads (MEDIUM)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-snapshot-reads`

- **Feature**: [features/snapshot-reads.md](features/snapshot-reads.md)

- **Purpose**: Implement the engine-agnostic per-Quota state read — the unified snapshot endpoint serving single,
  bulk-paginated, and end-user-restricted cases — with `currently_within_window` computation, PDP-scoped
  filtering, and no Policy attribution or aggregate headline numbers.

- **Depends On**: `cpt-cf-quota-enforcement-feature-quota-lifecycle`,
  `cpt-cf-quota-enforcement-feature-consumption-operations`

- **Scope**:
  - `POST /quota-enforcement/v1/snapshot` for `1..N` `(subject, metric)` filters with cursor pagination
  - Consumer-backed self-service reads for a `{type, id}` user subject and its tenant, returning every applicable
    Quota
  - The read-only guarantee (I3): a consumption Quota without a current period row reads as zero, and no read creates one

- **Out of scope**:
  - Admission verdicts (served by `evaluate_preview` in feature `consumption-operations`)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-quota-snapshot-read`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-bulk-quota-snapshot-read`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-end-user-quota-snapshot-read`

- **Design Principles Covered**:

  - None (read-only composition over earlier features)

- **Design Constraints Covered**:

  - None (constraints are owned by the features that introduce them)

- **Domain Model Entities**:
  - `Quota` (owned by quota-lifecycle; read here)
  - `Counter` (consumption) (owned by consumption-operations; read here)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-quota-enforcement-service`

- **API**:
  - POST /quota-enforcement/v1/snapshot
  - `QuotaEnforcementApi` (`snapshot`)

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-end-user-snapshot`

- **Data**:

  - None (reads existing counter and Quota rows; period materialization is owned by `consumption-operations`)

### 2.9 Notification Outbox & Dispatch (MEDIUM)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-notifications`

- **Feature**: [features/notifications.md](features/notifications.md)

- **Purpose**: Implement the `QuotaNotificationSinkV1` plugin contract and the outbox-backed dispatcher delivering
  the eight-kind event catalog at-least-once to every registered sink, with per-sink failure isolation,
  dead-lettering, and threshold-crossing emission rules.

- **Depends On**: `cpt-cf-quota-enforcement-feature-foundation`

- **Scope**:
  - Dispatch as a `toolkit-db` Outbox leased handler (framework-owned claiming, retries, acks, dead letters, and
    lease fencing); at-least-once delivery with per-sink timeout and permitted duplicates
  - `QuotaNotificationSinkV1` trait and `DispatchError` retry/dead-letter policy
  - Event catalog payloads and discriminators; threshold upward-transition semantics with per-period markers
  - Dispatch-failure and outbox telemetry

- **Out of scope**:
  - EventBus routing (P2 per PRD §13)
  - Event-producing state changes themselves (owned by their features; events ride the same-transaction outbox)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-notification-plugin`

- **Design Principles Covered**:

  - None (delivery is best-effort by requirement, not a new principle)

- **Design Constraints Covered**:

  - None (constraints are owned by the features that introduce them)

- **Domain Model Entities**:
  - `NotificationOutboxEvent`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-notification-dispatcher`

- **API**:
  - `QuotaNotificationSinkV1` plugin trait (`id`, `dispatch`)

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-notification-dispatch`

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.10 Bulk Quota CRUD (MEDIUM)

- [ ] `p2` - **ID**: `cpt-cf-quota-enforcement-feature-bulk-quota-crud`

- **Feature**: [features/bulk-quota-crud.md](features/bulk-quota-crud.md)

- **Purpose**: Implement the transactional bulk Quota endpoints — `bulk_create_quotas`, `bulk_update_quotas`,
  `bulk_deactivate_quotas` — with envelope idempotency, all-or-nothing semantics, batch-size limits, and per-item
  failure attribution, so Quota Manager materialization flows stop composing per-Quota calls with client-side
  compensation.

- **Depends On**: `cpt-cf-quota-enforcement-feature-quota-lifecycle`,
  `cpt-cf-quota-enforcement-feature-lease-operations`

- **Scope**:
  - The three bulk endpoints with envelope idempotency keys and `BULK_TOO_LARGE` enforcement
  - Atomic lease resolution across every Quota deactivated in a batch

- **Out of scope**:
  - Any partial-success mode (all-or-nothing is the contract)

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-quota-enforcement-fr-bulk-quota-crud`

- **Design Principles Covered**:

  - None (extends feature `quota-lifecycle` semantics unchanged)

- **Design Constraints Covered**:

  - None (constraints are owned by the features that introduce them)

- **Domain Model Entities**:
  - `Quota` (owned by quota-lifecycle; mutated in bulk here)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-quota-management-service`

- **API**:
  - POST /quota-enforcement/v1/quotas/bulk-create
  - POST /quota-enforcement/v1/quotas/bulk-update
  - POST /quota-enforcement/v1/quotas/bulk-deactivate

- **Sequences**:

  - None (per PRD §5.2 the bulk endpoints follow the single-item sequences with an envelope wrapper)

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.11 Rate Quotas (LOW)

- [ ] `p3` - **ID**: `cpt-cf-quota-enforcement-feature-rate-quotas`

- **Feature**: [features/rate-quotas.md](features/rate-quotas.md)

- **Purpose**: Activate the reserved `rate` quota type per the P3 field contract (`rate`, `burst_capacity`,
  `smoothing_window`), with `RATE_WINDOW_EXHAUSTED` denials carrying a `Retry-After` floor and migration-free
  coexistence with the P1 allocation and consumption types; the burst mechanism stays open until the PRD §13
  decision closes.

- **Depends On**: `cpt-cf-quota-enforcement-feature-consumption-operations`

- **Scope**:
  - Rate-quota data-model activation (`rate_spec`) without migrating existing Quotas
  - Rate admission semantics per the P3 field contract in PRD §5.3

- **Out of scope**:
  - Burst mechanism selection (bucket vs window) until the PRD §13 open question is resolved

- **Requirements Covered**:

  - [ ] `p3` - `cpt-cf-quota-enforcement-fr-quota-type-rate-declared`

- **Design Principles Covered**:

  - None (applies existing evaluation principles to a new quota type)

- **Design Constraints Covered**:

  - None (constraints are owned by the features that introduce them)

- **Domain Model Entities**:
  - `Quota` (owned by quota-lifecycle; extended with `rate_spec`)
  - `Counter` (consumption) (owned by consumption-operations; extended for rate windows)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-evaluation-orchestrator`

- **API**:
  - Existing quota CRUD and evaluation endpoints accept `type = rate` once activated

- **Sequences**:

  - None (rate admission reuses the debit sequence; a dedicated sequence is a P3 DESIGN concern)

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.12 Quota Resolver Plugin (HIGH)

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-resolver-plugin`

- **Feature**: [features/resolver-plugin.md](features/resolver-plugin.md)

- **Purpose**: Make QE the quota resolver's backend: implement `QuotaResolverPluginClient` inside the gear, register
  it as a quota resolver plugin instance, and run debit, lease, return, and quota state through the QE operations
  built over trusted admission (ADR-0009).

- **Depends On**: `cpt-cf-quota-enforcement-feature-lease-operations`,
  `cpt-cf-quota-enforcement-feature-snapshot-reads`

- **Scope**:
  - Plugin registration in ClientHub and types-registry under the quota resolver plugin type
  - Conversion of resolver requests to QE requests and of QE results and errors to resolver results
  - Operations without a `SecurityContext`; the platform identity of the resolver's caller recorded as the
    operation-log actor
  - Quota state served from the snapshot read with the Quota metadata and without the fields only operators see
    (enforcement mode, validity window)

- **Out of scope**:
  - Trusted admission itself (feature `foundation`); return and the charge ledger (feature `consumption-operations`)
  - Several metrics in one operation (a P2 quota resolver requirement)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-resolver-plugin`

- **Design Principles Covered**:

  - None (applies the foundation admission and the operation features unchanged)

- **Design Constraints Covered**:

  - None (constraints are owned by the features that introduce them)

- **Domain Model Entities**:
  - None (converts quota resolver SDK types to the entities of earlier features)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-resolver-plugin`

- **API**:
  - `QuotaResolverPluginClient` (quota resolver SDK), implemented in process; no QE route

- **Sequences**:

  - None (the plugin path runs the debit, lease, return, and snapshot flows of earlier features)

- **Data**:

  - None (no table of its own)

### 2.13 Deliberate Omissions & Shared Elements

The following defined IDs are intentionally not carried as feature coverage entries, and the listed shared references
are intentional:

- **Interfaces and contracts** (`cpt-cf-quota-enforcement-interface-*`, `cpt-cf-quota-enforcement-contract-*`): the
  interface and contract surfaces are delivered incrementally by the features whose API sections name them (traits and
  routes land with their owning feature); they are artifact-level declarations, not independently implementable work
  packages.
- **Actors and use cases** (`cpt-cf-quota-enforcement-actor-*`, `cpt-cf-quota-enforcement-usecase-*`): PRD context
  elements; they are validated through the requirements they motivate, which are fully covered above.
- **ADRs** (`cpt-cf-quota-enforcement-adr-*`): decisions constraining several features at once; they are traced from
  the DESIGN sections the features cover rather than assigned to one feature.
- **`cpt-cf-quota-enforcement-tech-stack`, `cpt-cf-quota-enforcement-design-main`,
  `cpt-cf-quota-enforcement-topology`**: document-wide DESIGN elements realized cumulatively by all
  features; assigning them to a single feature would misstate ownership.
- **`cpt-cf-quota-enforcement-fr-quota-type-rate-declared`** is covered only by the `p3` rate-quotas feature by
  design: its P1 obligation (rejection) is a distinct requirement covered in quota-lifecycle.
- **Shared references with explicit reason** (allowed overlap): `cpt-cf-quota-enforcement-db-schema` appears in every
  feature that adds tables to the one versioned schema; `cpt-cf-quota-enforcement-component-gateway` is stood up by
  `cpt-cf-quota-enforcement-feature-foundation` and extended with ingress validation by
  `cpt-cf-quota-enforcement-feature-projection-contracts`; `cpt-cf-quota-enforcement-component-evaluation-orchestrator`
  is established by `cpt-cf-quota-enforcement-feature-consumption-operations`, consumes the subject admission
  that `cpt-cf-quota-enforcement-feature-projection-contracts` defines, and is extended by
  `cpt-cf-quota-enforcement-feature-batch-debit` and `cpt-cf-quota-enforcement-feature-rate-quotas`;
  `cpt-cf-quota-enforcement-component-quota-enforcement-service` is shared by
  `cpt-cf-quota-enforcement-feature-consumption-operations` and `cpt-cf-quota-enforcement-feature-snapshot-reads`;
  `cpt-cf-quota-enforcement-component-quota-management-service` is shared by
  `cpt-cf-quota-enforcement-feature-quota-lifecycle` and `cpt-cf-quota-enforcement-feature-bulk-quota-crud`. In each
  case the earlier feature owns the component; the later one extends it through the same contract.

---

## 3. Feature Dependencies

```text
cpt-cf-quota-enforcement-feature-foundation
    ↓
    ├─→ cpt-cf-quota-enforcement-feature-projection-contracts
    │       ↓
    │       ├─→ cpt-cf-quota-enforcement-feature-resolution-policy-engine
    │       └─→ cpt-cf-quota-enforcement-feature-quota-lifecycle
    │       ↓ (both joined)
    │           cpt-cf-quota-enforcement-feature-consumption-operations
    │               ↓
    │               ├─→ cpt-cf-quota-enforcement-feature-lease-operations
    │               │       ├─→ cpt-cf-quota-enforcement-feature-bulk-quota-crud (also needs quota-lifecycle)
    │               │       └─→ cpt-cf-quota-enforcement-feature-resolver-plugin (also needs snapshot-reads)
    │               ├─→ cpt-cf-quota-enforcement-feature-batch-debit
    │               ├─→ cpt-cf-quota-enforcement-feature-snapshot-reads (also needs quota-lifecycle)
    │               └─→ cpt-cf-quota-enforcement-feature-rate-quotas
    └─→ cpt-cf-quota-enforcement-feature-notifications
```

**Dependency Rationale**:

- `cpt-cf-quota-enforcement-feature-projection-contracts` requires
  `cpt-cf-quota-enforcement-feature-foundation`: projection-contracts starts its catalogue reads inside the bootstrap
  hook foundation provides, and its ingress validation lives in the Gateway that foundation registers.
- `cpt-cf-quota-enforcement-feature-resolution-policy-engine` requires
  `cpt-cf-quota-enforcement-feature-projection-contracts` because a Policy write needs a served catalogue entry of
  its metric and checks a `cel` Policy against the schema of the metric type.
- `cpt-cf-quota-enforcement-feature-quota-lifecycle` requires the foundation storage plugin for transactional CRUD,
  and `cpt-cf-quota-enforcement-feature-projection-contracts` because Quota creation checks that the metric admits the
  Quota's subject type and the conditions of its metadata against the metric type.
- `cpt-cf-quota-enforcement-feature-consumption-operations` joins
  `cpt-cf-quota-enforcement-feature-quota-lifecycle` (Quotas and counters to mutate) with
  `cpt-cf-quota-enforcement-feature-resolution-policy-engine` (the Decision it applies).
- `cpt-cf-quota-enforcement-feature-lease-operations`, `cpt-cf-quota-enforcement-feature-batch-debit`, and
  `cpt-cf-quota-enforcement-feature-rate-quotas` extend the orchestrator pipeline that
  `cpt-cf-quota-enforcement-feature-consumption-operations` establishes.
- `cpt-cf-quota-enforcement-feature-snapshot-reads` needs Quota records from
  `cpt-cf-quota-enforcement-feature-quota-lifecycle` and the counter/period materialization from
  `cpt-cf-quota-enforcement-feature-consumption-operations`.
- `cpt-cf-quota-enforcement-feature-bulk-quota-crud` requires
  `cpt-cf-quota-enforcement-feature-quota-lifecycle` for the single-item CRUD it wraps and
  `cpt-cf-quota-enforcement-feature-lease-operations` because bulk deactivation atomically resolves the active leases
  of every Quota in the batch.
- `cpt-cf-quota-enforcement-feature-resolver-plugin` requires
  `cpt-cf-quota-enforcement-feature-lease-operations` (debit, lease, and return, with the consumption-operations
  feature beneath it) and `cpt-cf-quota-enforcement-feature-snapshot-reads` (quota state); it runs them over the
  foundation's trusted admission.
- `cpt-cf-quota-enforcement-feature-notifications` requires only the foundation outbox integration; the dispatcher is fenced by the Outbox lease itself, and it is
  independent of the evaluation pipeline and can proceed in parallel with
  `cpt-cf-quota-enforcement-feature-projection-contracts` and everything downstream.
- Parallelization: after foundation, `cpt-cf-quota-enforcement-feature-notifications` and
  `cpt-cf-quota-enforcement-feature-projection-contracts` proceed in parallel; after projection-contracts,
  `cpt-cf-quota-enforcement-feature-resolution-policy-engine` and then
  `cpt-cf-quota-enforcement-feature-quota-lifecycle` follow in that order; after consumption-operations, `cpt-cf-quota-enforcement-feature-lease-operations`,
  `cpt-cf-quota-enforcement-feature-batch-debit`, and `cpt-cf-quota-enforcement-feature-snapshot-reads` proceed in
  parallel; `cpt-cf-quota-enforcement-feature-resolver-plugin` follows lease-operations and snapshot-reads.
