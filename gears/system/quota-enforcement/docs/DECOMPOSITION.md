<!-- Created: 2026-08-24 by Constructor Tech -->

# Decomposition: Quota Enforcement

**Overall implementation status:**

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-status-implementation`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Gear Foundation, Storage & Coordination ⏳ HIGH](#21-gear-foundation-storage--coordination--high)
  - [2.2 Projection Contracts & Subject Resolution ⏳ HIGH](#22-projection-contracts--subject-resolution--high)
  - [2.3 Quota Lifecycle & Metadata ⏳ HIGH](#23-quota-lifecycle--metadata--high)
  - [2.4 Resolution Policy & Engine ⏳ HIGH](#24-resolution-policy--engine--high)
  - [2.5 Consumption Operations & Idempotency ⏳ HIGH](#25-consumption-operations--idempotency--high)
  - [2.6 Lease Operations ⏳ HIGH](#26-lease-operations--high)
  - [2.7 Batch Debit ⏳ MEDIUM](#27-batch-debit--medium)
  - [2.8 Quota Snapshot Reads ⏳ MEDIUM](#28-quota-snapshot-reads--medium)
  - [2.9 Notification Outbox & Dispatch ⏳ MEDIUM](#29-notification-outbox--dispatch--medium)
  - [2.10 Bulk Quota CRUD ⏳ MEDIUM](#210-bulk-quota-crud--medium)
  - [2.11 Rate Quotas ⏳ LOW](#211-rate-quotas--low)
  - [2.12 Deliberate Omissions & Shared Elements](#212-deliberate-omissions--shared-elements)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

The DESIGN is decomposed into eleven features along its plugin seams and evaluation pipeline. The strategy is
dependency-ordered vertical slices: the foundation feature stands up the gear skeleton, the storage and coordination
plugin contracts, authorization, and tenant isolation; the projection-contracts feature adds the declarative GTS
request surface those operations validate against; lifecycle, policy/engine, and the operation features then build the
evaluation pipeline in the order the `EvaluationOrchestrator` executes it. Read surfaces, notifications, and the
deferred P2/P3 capabilities close the list. Every P1 functional requirement, NFR, design principle, constraint,
component, sequence, and the database schema is covered by exactly the features that implement it; deferred
requirements (`p2`/`p3`) are carried by dedicated features so no P1 feature depends on deferred work. Each feature is
a single implementation phase; sub-decomposition below the feature level happens in the per-feature FEATURE documents,
not here.

## 2. Entries

### 2.1 Gear Foundation, Storage & Coordination ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-foundation`

- **Purpose**: Stand up the `quota-enforcement` gear and SDK crates, the `QuotaEnforcementStoragePluginV1` contract
  with its `toolkit-db` reference plugin, the `CoordinationPluginV1` contract with its storage-backed default, gear
  bootstrap (schema check, seeded global Policy, config-table defaults, and the bootstrap hook later features extend),
  two-phase PDP authorization, tenant isolation, and the gear-specific telemetry conventions. Every later feature
  builds on these seams.

- **Depends On**: None

- **Scope**:
  - `quota-enforcement` and `quota-enforcement-sdk` crate skeletons registered in the workspace
  - `QuotaEnforcementStoragePluginV1` trait, closed `StorageError` enum, invariants I1–I13, `toolkit-db` plugin
  - `CoordinationPluginV1` trait (`try_lock`/`renew`/`release`), storage-backed default implementation
  - Gateway REST registration into the platform `api-gateway`, DTO validation shell, phase-1 PDP admission with LRU
    cache and fail-closed posture, phase-2 constraint propagation
  - Tenant isolation filters at gateway and storage layers
  - Bootstrap: schema-version check, seeded `global` Policy row, default config-table rows, fail-fast posture
  - Telemetry emission conventions with bounded label cardinality

- **Out of scope**:
  - Projection contracts and their bootstrap consistency checks (feature `projection-contracts`)
  - Any evaluation, lease, or notification behavior (later features)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-pluggable-storage`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-authorization`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-tenant-isolation`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-telemetry`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-authentication`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-authorization`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-tenant-isolation-integrity`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-fault-tolerance`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-recovery`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-availability`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-subject-scale`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-quota-density`

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
  - REST route registration under `/v1/quota-enforcement/...` (routes land with their owning features)
  - `QuotaEnforcementStoragePluginV1`, `CoordinationPluginV1` plugin traits in the SDK crate

- **Sequences**:

  - None (cross-cutting; every later sequence exercises this foundation)

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.2 Projection Contracts & Subject Resolution ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-projection-contracts`

- **Purpose**: Register the abstract QE subject/resource/Quota-attribute bases and the scope-discriminator type,
  resolve owner projections into the immutable `ProjectionContractCatalog`, validate every evaluation request at
  Gateway ingress against the declared contracts, and derive subject identity server-side from `SecurityContext`.
  This is the declarative contract surface the whole evaluation pipeline trusts.

- **Depends On**: `cpt-cf-quota-enforcement-feature-foundation`

- **Scope**:
  - Bootstrap registration of the abstract bases and the P1 `user`/`tenant` scope well-known instances
  - `SubjectProjectionResolver` implementations and the bootstrap consistency set (1:1 resolver mapping, admitted
    metric registration and derivation, `(metric, scope)` uniqueness, anonymous/nil rejection)
  - `ProjectionContractCatalog` build and publication, including the authoritative reverse index from each metric
    to its complete configured subject-projection set; catalogue-membership checks on Quota and Policy writes
  - Server-side projection selection from that reverse index: the caller-declared projection is validated as a
    member of the metric's set and cannot narrow the applicable set
  - Ingress validation: registered concrete types, required `metadata`, admitted metric, structural `caller_type`
    validation and its exclusion from `EvaluationContext`
  - Contract-validation telemetry counters

- **Out of scope**:
  - Quota records themselves (feature `quota-lifecycle`)
  - Engine consumption of projection metadata (feature `resolution-policy-engine`)
  - Breaking projection-version activation (out of P1 per PRD §5.2)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-projection-contracts`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-contract-validation`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-subject-type-registry`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-subject-resolution`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-principle-declarative-projection-contracts`
  - [ ] `p1` - `cpt-cf-quota-enforcement-principle-server-derived-identity`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-constraint-types-registry-delegation`

- **Domain Model Entities**:
  - `SubjectScope`
  - `SubjectProjectionContract`
  - `ResourceProjectionContract`
  - `QuotaAttributeContract`
  - `ProjectionContractCatalog`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-gateway`
  - [ ] `p1` - `cpt-cf-quota-enforcement-component-evaluation-orchestrator`

- **API**:
  - Evaluation request contract fields (`subject_projection_type`, `subject_metadata`, `resource_projection_type`,
    `resource_metadata`, `caller_type`) on every write/preview DTO
  - No registration endpoint: contracts are published through `types-registry`

- **Sequences**:

  - None (ingress validation is a step inside every evaluation sequence)

- **Data**:

  - None (contracts are registry-resident; no QE-side table)

### 2.3 Quota Lifecycle & Metadata ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-quota-lifecycle`

- **Purpose**: Implement the Quota entity and its lifecycle — create, update, deactivate, read — with metric
  validation against `types-registry`, cap and validity-window semantics, opaque size-capped metadata, the closed
  `enforcement_mode` and `source` enums, rate-type rejection, and the deactivation cascade that resolves active
  leases atomically.

- **Depends On**: `cpt-cf-quota-enforcement-feature-foundation`,
  `cpt-cf-quota-enforcement-feature-projection-contracts`

- **Scope**:
  - `QuotaManagementService` with transactional CRUD via the storage plugin
  - Metric existence/kind/mode validation through `TypesRegistryClient` with LRU cache, fail-closed
  - Cap semantics (`CAP_MUST_BE_NON_NEGATIVE`, `cap = 0`, `cap = null`, `CAP_BELOW_CONSUMED` at commit time)
  - Quota Metadata validation against the owner's Quota-attribute contract at write time only
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

  - None (applies the foundation and projection principles; introduces none of its own)

- **Design Constraints Covered**:

  - None (constraints are owned by the features that introduce them)

- **Domain Model Entities**:
  - `Quota`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-quota-management-service`

- **API**:
  - POST /v1/quota-enforcement/quotas
  - GET /v1/quota-enforcement/quotas/{id}
  - PATCH /v1/quota-enforcement/quotas/{id}
  - POST /v1/quota-enforcement/quotas/{id}/deactivate
  - GET /v1/quota-enforcement/quotas
  - `QuotaManagerClientV1` Quota methods (`create_quota`, `update_quota`, `deactivate_quota`, `read_quotas`)

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-quota-create`
  - `cpt-cf-quota-enforcement-seq-quota-deactivate-cascade`

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.4 Resolution Policy & Engine ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-resolution-policy-engine`

- **Purpose**: Implement the Quota Resolution Policy entity with immutable versioning and rollback, the
  `QuotaResolutionEngineV1` plugin contract with the `most-restrictive-wins` and `cel` built-ins, multi-Quota
  arbitration with strict Debit-Plan invariants, cascade and attribute-based selection expressiveness, and the
  hard-cap denial contract.

- **Depends On**: `cpt-cf-quota-enforcement-feature-foundation`,
  `cpt-cf-quota-enforcement-feature-projection-contracts`

- **Scope**:
  - `PolicyService` with scope precedence (per-metric over `global`) and version lifecycle
    (`active`/`superseded`/`rolled_back`/`deleted`), optimistic `if_match_version` concurrency
  - `EngineRegistry` with fail-fast bootstrap registration
  - Engine contract: `id`/`validate_config`/`evaluate`, `ValidatedConfig` cache keyed by `(policy_id, policy_version)`
  - Debit-Plan invariant enforcement at the Engine boundary with violation telemetry
  - `most-restrictive-wins` binding-Quota selection and validity-window prefilter; sandboxed cost-bounded `cel`
  - Static CEL checking against snapshotted request and Quota-attribute schemas, including pair compatibility
  - Operator-only Policy surface (`QuotaOperatorClientV1`)

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
  - POST /v1/quota-enforcement/policies
  - GET /v1/quota-enforcement/policies/{id}
  - GET /v1/quota-enforcement/policies/{id}/versions
  - PATCH /v1/quota-enforcement/policies/{id}
  - POST /v1/quota-enforcement/policies/{id}/rollback
  - DELETE /v1/quota-enforcement/policies/{id}
  - `QuotaOperatorClientV1` Policy methods; `QuotaResolutionEngineV1` plugin trait

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-policy-version-update`

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.5 Consumption Operations & Idempotency ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-consumption-operations`

- **Purpose**: Implement the `EvaluationOrchestrator` pipeline and the single-shot counter operations — debit,
  credit, rollback, and read-only preview — over allocation and consumption counters, with calendar-aligned UTC
  periods, deterministic rollover and settlement semantics, and the four-component idempotency scope that makes
  every write replay-safe.

- **Depends On**: `cpt-cf-quota-enforcement-feature-quota-lifecycle`,
  `cpt-cf-quota-enforcement-feature-resolution-policy-engine`

- **Scope**:
  - `QuotaEnforcementService` entry points and the canonical orchestrator pipeline (resolution → idempotency lookup →
    locked read → Policy lookup → Engine → invariant check → mutation → persist → outbox → commit)
  - Allocation and consumption counter shapes with lazy period-row materialization and threshold-marker reset
  - Debit applying Engine Debit Plans atomically; credit against a named Quota; rollback by original idempotency key
    with settlement-keyed closure; preview with no persisted state
  - `INVALID_AMOUNT` fail-fast ordering; `IDEMPOTENCY_PAYLOAD_MISMATCH`; retention via `RetentionSweeper`
  - Period rollover event ordering and the credit/rollback closure asymmetry

- **Out of scope**:
  - Two-phase holds (feature `lease-operations`)
  - Multi-item envelopes (feature `batch-debit`)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-debit`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-credit`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-rollback`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-evaluate-preview`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-idempotency`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-period-semantics`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-period-rollover`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-quota-type-allocation`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-quota-type-consumption`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-evaluation-latency`
  - [ ] `p1` - `cpt-cf-quota-enforcement-nfr-throughput`
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

- **Design Components**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-component-quota-enforcement-service`
  - [ ] `p1` - `cpt-cf-quota-enforcement-component-evaluation-orchestrator`
  - [ ] `p1` - `cpt-cf-quota-enforcement-component-idempotency-cache`
  - [ ] `p1` - `cpt-cf-quota-enforcement-component-retention-sweeper`

- **API**:
  - POST /v1/quota-enforcement/operations/debit
  - POST /v1/quota-enforcement/operations/credit
  - POST /v1/quota-enforcement/operations/rollback
  - POST /v1/quota-enforcement/operations/preview
  - `QuotaEnforcementClientV1` (`debit`, `rollback`, `evaluate_preview`); `QuotaManagerClientV1` (`credit`)

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-debit`
  - `cpt-cf-quota-enforcement-seq-credit`
  - `cpt-cf-quota-enforcement-seq-rollback`
  - `cpt-cf-quota-enforcement-seq-evaluate-preview`
  - `cpt-cf-quota-enforcement-seq-idempotency-replay`
  - `cpt-cf-quota-enforcement-seq-period-rollover`

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.6 Lease Operations ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-lease-operations`

- **Purpose**: Implement the two-phase lease protocol — atomic multi-Quota acquisition, commit with unused-capacity
  return, release, and TTL auto-release — under the lazy-expiry semantic that keeps correctness independent of
  sweeper liveness, with acquisition-period attribution, contention timeouts, and the per-`(tenant, metric)`
  active-lease cap.

- **Depends On**: `cpt-cf-quota-enforcement-feature-consumption-operations`

- **Scope**:
  - `LeaseManager` state machine (`Active` → `Committed`/`Released`/`AutoReleased`/`ResolvedByDeactivation`)
  - Atomic multi-Quota hold acquisition in lexicographic `quota_id` order; TTL bounds without clamping
  - Cross-period and cross-validity commit attribution to the acquisition period
  - Lazy semantic release on every read/write path; `LeaseSweeper` physical reclamation under a coordination lock
  - Contention timeout and active-lease-cap enforcement with their telemetry

- **Out of scope**:
  - Lease-resolution behavior of Quota deactivation (owned by feature `quota-lifecycle`)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-lease-acquire`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-lease-commit`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-lease-release`
  - [ ] `p1` - `cpt-cf-quota-enforcement-fr-lease-timeout`

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
  - POST /v1/quota-enforcement/leases
  - POST /v1/quota-enforcement/leases/{token}/commit
  - POST /v1/quota-enforcement/leases/{token}/release
  - `QuotaEnforcementClientV1` (`acquire_lease`, `commit_lease`, `release_lease`)

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-lease-acquire`
  - `cpt-cf-quota-enforcement-seq-lease-commit`
  - `cpt-cf-quota-enforcement-seq-lease-release`
  - `cpt-cf-quota-enforcement-seq-lease-auto-release`

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.7 Batch Debit ⏳ MEDIUM

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-batch-debit`

- **Purpose**: Implement the multi-item debit envelope for single logical operations that consume several metrics
  at once: atomic all-or-nothing evaluation where each item observes the running batch state, envelope idempotency,
  the batch-level evaluation timeout, and P1 rejection of the deferred `independent` mode.

- **Depends On**: `cpt-cf-quota-enforcement-feature-consumption-operations`

- **Scope**:
  - `apply_batch_debit` storage primitive and envelope/per-item idempotency keys
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
  - POST /v1/quota-enforcement/operations/batch-debit
  - `QuotaEnforcementClientV1` (`batch_debit`)

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-batch-debit`

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.8 Quota Snapshot Reads ⏳ MEDIUM

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-snapshot-reads`

- **Purpose**: Implement the engine-agnostic per-Quota state read — the unified snapshot endpoint serving single,
  bulk-paginated, and end-user-restricted cases — with `currently_within_window` computation, PDP-scoped
  filtering, and no Policy attribution or aggregate headline numbers.

- **Depends On**: `cpt-cf-quota-enforcement-feature-quota-lifecycle`,
  `cpt-cf-quota-enforcement-feature-consumption-operations`

- **Scope**:
  - `POST /v1/quota-enforcement/snapshot` for `1..N` `(subject, metric)` filters with cursor pagination
  - End-user restriction to the forwarded context's own user/tenant projections, returning every applicable Quota
  - Lazy period-row materialization as the single read-path write exception

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
  - POST /v1/quota-enforcement/snapshot
  - `QuotaEnforcementClientV1` (`snapshot`)

- **Sequences**:

  - `cpt-cf-quota-enforcement-seq-end-user-snapshot`

- **Data**:

  - None (reads existing counter and Quota rows; period materialization is owned by `consumption-operations`)

### 2.9 Notification Outbox & Dispatch ⏳ MEDIUM

- [ ] `p1` - **ID**: `cpt-cf-quota-enforcement-feature-notifications`

- **Purpose**: Implement the `QuotaNotificationSinkV1` plugin contract and the outbox-backed dispatcher delivering
  the eight-kind event catalog at-least-once to every registered sink, with per-sink failure isolation,
  dead-lettering, and threshold-crossing emission rules.

- **Depends On**: `cpt-cf-quota-enforcement-feature-foundation`

- **Scope**:
  - `notification_outbox` drain under a coordination lock; at-least-once dispatch with per-sink timeout
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

### 2.10 Bulk Quota CRUD ⏳ MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-quota-enforcement-feature-bulk-quota-crud`

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
  - POST /v1/quota-enforcement/quotas/bulk-create
  - POST /v1/quota-enforcement/quotas/bulk-update
  - POST /v1/quota-enforcement/quotas/bulk-deactivate

- **Sequences**:

  - None (per PRD §5.2 the bulk endpoints follow the single-item sequences with an envelope wrapper)

- **Data**:

  - [ ] `p1` - `cpt-cf-quota-enforcement-db-schema`

### 2.11 Rate Quotas ⏳ LOW

- [ ] `p3` - **ID**: `cpt-cf-quota-enforcement-feature-rate-quotas`

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

### 2.12 Deliberate Omissions & Shared Elements

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
  is established by `cpt-cf-quota-enforcement-feature-consumption-operations` and extended by
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
    │       ├─→ cpt-cf-quota-enforcement-feature-quota-lifecycle
    │       └─→ cpt-cf-quota-enforcement-feature-resolution-policy-engine
    │               ↓ (joined with quota-lifecycle)
    │           cpt-cf-quota-enforcement-feature-consumption-operations
    │               ↓
    │               ├─→ cpt-cf-quota-enforcement-feature-lease-operations
    │               │       └─→ cpt-cf-quota-enforcement-feature-bulk-quota-crud (also needs quota-lifecycle)
    │               ├─→ cpt-cf-quota-enforcement-feature-batch-debit
    │               ├─→ cpt-cf-quota-enforcement-feature-snapshot-reads (also needs quota-lifecycle)
    │               └─→ cpt-cf-quota-enforcement-feature-rate-quotas
    └─→ cpt-cf-quota-enforcement-feature-notifications
```

**Dependency Rationale**:

- `cpt-cf-quota-enforcement-feature-projection-contracts` requires
  `cpt-cf-quota-enforcement-feature-foundation`: projection-contracts publishes the catalogue and runs its
  consistency checks inside the bootstrap hook foundation provides, and its ingress validation lives in the
  Gateway that foundation registers.
- `cpt-cf-quota-enforcement-feature-quota-lifecycle` requires the foundation storage plugin for transactional CRUD
  and `cpt-cf-quota-enforcement-feature-projection-contracts` because Quota creation validates its owner projection,
  scope, and admitted metric.
- `cpt-cf-quota-enforcement-feature-resolution-policy-engine` requires
  `cpt-cf-quota-enforcement-feature-projection-contracts` because Policy validation type-checks CEL against the
  snapshotted contract schemas.
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
- `cpt-cf-quota-enforcement-feature-notifications` requires only the foundation outbox and coordination lock; it is
  independent of the evaluation pipeline and can proceed in parallel with
  `cpt-cf-quota-enforcement-feature-projection-contracts` and everything downstream.
- Parallelization: after foundation, `cpt-cf-quota-enforcement-feature-notifications` and
  `cpt-cf-quota-enforcement-feature-projection-contracts` proceed in parallel; after projection-contracts,
  `cpt-cf-quota-enforcement-feature-quota-lifecycle` and
  `cpt-cf-quota-enforcement-feature-resolution-policy-engine` proceed in parallel; after consumption-operations,
  `cpt-cf-quota-enforcement-feature-lease-operations`, `cpt-cf-quota-enforcement-feature-batch-debit`, and
  `cpt-cf-quota-enforcement-feature-snapshot-reads` proceed in parallel.
