<!--
cpt:
  version: 0.2.1
  updated: 2026-06-02
-->

# Feature: Usage Emission

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Explicit Non-Applicability](#15-explicit-non-applicability)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Emit Record (single)](#emit-record-single)
  - [Emit Records Batch](#emit-records-batch)
  - [Compensation Emission](#compensation-emission)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Attribution and PDP Authorization](#attribution-and-pdp-authorization)
  - [Semantics Enforcement on Ingest](#semantics-enforcement-on-ingest)
  - [Metadata Size-Cap Enforcement](#metadata-size-cap-enforcement)
  - [Catalog Existence and Kind Lookup](#catalog-existence-and-kind-lookup)
- [4. States (CDSL)](#4-states-cdsl)
  - [Usage Record Ingestion Lifecycle State Machine](#usage-record-ingestion-lifecycle-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [FR: Ingestion](#fr-ingestion)
  - [FR: Idempotency](#fr-idempotency)
  - [FR: Record Metadata](#fr-record-metadata)
  - [FR: UsageType Existence and Semantics](#fr-usagetype-existence-and-semantics)
  - [FR: Counter Semantics](#fr-counter-semantics)
  - [FR: Gauge Semantics](#fr-gauge-semantics)
  - [FR: Tenant Attribution](#fr-tenant-attribution)
  - [FR: Resource Attribution](#fr-resource-attribution)
  - [FR: Subject Attribution](#fr-subject-attribution)
  - [FR: Ingestion Authorization](#fr-ingestion-authorization)
  - [FR: Usage Compensation — Flow](#fr-usage-compensation--flow)
  - [FR: Usage Compensation — Value Matrix](#fr-usage-compensation--value-matrix)
  - [FR: Usage Compensation — L1 corrects_id](#fr-usage-compensation--l1-corrects_id)
  - [FR: Usage Compensation — Concurrency](#fr-usage-compensation--concurrency)
  - [FR: Usage Compensation — No Business Logic](#fr-usage-compensation--no-business-logic)
  - [NFR: Throughput](#nfr-throughput)
  - [NFR: Throughput Profile](#nfr-throughput-profile)
  - [NFR: Ingestion Latency](#nfr-ingestion-latency)
  - [NFR: Workload Isolation](#nfr-workload-isolation)
  - [Principle: Idempotency by Key](#principle-idempotency-by-key)
  - [Principle: Semantics Enforcement](#principle-semantics-enforcement)
  - [Principle: Fail Closed](#principle-fail-closed)
  - [Principle: Pluggable Storage](#principle-pluggable-storage)
  - [Constraint: No Business Logic](#constraint-no-business-logic)
  - [Constraint: NFR Thresholds](#constraint-nfr-thresholds)
  - [ADR: Caller-supplied Attribution](#adr-caller-supplied-attribution)
  - [ADR: Mandatory Idempotency](#adr-mandatory-idempotency)
  - [Component: Ingestion Gateway](#component-ingestion-gateway)
  - [Sequence: Emit Usage Record](#sequence-emit-usage-record)
  - [Entity: Usage Record](#entity-usage-record)
  - [Entity: Record Metadata](#entity-record-metadata)
  - [Entity: Resource Ref](#entity-resource-ref)
  - [Entity: Subject Ref](#entity-subject-ref)
  - [Entity: Idempotency Key](#entity-idempotency-key)
  - [Entity: UsageType](#entity-usagetype)
  - [Entity: Security Context](#entity-security-context)
  - [API: POST /usage-collector/v1/records](#api-post-usage-collectorv1records)
  - [§2.3-item → DoD-ID Coverage Matrix](#23-item--dod-id-coverage-matrix)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-featstatus-usage-emission`

<!-- reference to DECOMPOSITION entry -->

- [ ] `p2` - `cpt-cf-usage-collector-feature-usage-emission`

## 1. Feature Context

### 1.1 Overview

Provides the single, contract-first write path for at-least-once ingestion of usage records from authenticated callers. Every emit — single or batched, REST or SDK — flows through the Ingestion Gateway.

The `cpt-cf-usage-collector-component-ingestion-gateway` accepts the caller's `SecurityContext` (resolved upstream by the ToolKit gateway on REST as `Extension<SecurityContext>` populated via `OperationBuilder::authenticated()`, or supplied verbatim by the in-process caller on the SDK trait). It authorizes every record's full attribution tuple (tenant, resource, optional subject, UsageType `gts_id`) — together with the calling-gear identity carried in `SecurityContext` — through the per-component `authz_scope` helper wrapping `cpt-cf-usage-collector-contract-authz-resolver` fail-closed.

UsageType existence and the declared `metadata_fields` are resolved by dispatching `get_usage_type` against `cpt-cf-usage-collector-contract-storage-plugin` per record; counter / gauge classification is derived at the call site from the `gts_id` prefix. Semantics-dependent invariants are enforced (counter records reject negative deltas, gauges accept point-in-time values as-is), the fixed 8 KiB `RecordMetadata` size cap is enforced, and the validated record is dispatched through the Plugin SPI for durable persistence under the dedup composite `(tenant_id, gts_id, idempotency_key)`.

This is the only write path into `usage_records`; aggregation, query, deactivation, and audit-ledger semantics are owned elsewhere.

**Consistency posture (ingestion ack vs. query visibility).** The synchronous `Acknowledged` outcome returned by this feature is the ONLY surface that binds the gear-level consistency floor for write-derived state: durability and `(tenant_id, gts_id, idempotency_key)` dedup-tuple visibility on the ingestion path are guaranteed at ack.

Visibility of the same record through the read surfaces owned by `cpt-cf-usage-collector-feature-usage-query` and the usage-type-catalog reads owned by `cpt-cf-usage-collector-feature-usage-type-lifecycle` is governed separately by `cpt-cf-usage-collector-nfr-query-freshness` and `cpt-cf-usage-collector-adr-consistency-contract` (ADR-0011): eventually consistent with no upper bound at the gear floor, plugin-bound by the active plugin's published ceiling.

Source gears that need same-request outcome (admission control, post-emit summary, immediate-readback dashboards) MUST consume the ingestion ack this feature returns; they MUST NOT round-trip through the Query SPI for that purpose. Full contract: DESIGN [§3.10](../DESIGN.md#310-consistency-contract).

### 1.2 Purpose

This feature exists so that at-least-once delivery of usage records is uniformly safe across counter and gauge kinds — caller-supplied idempotency keys absorb retries without inflating counter totals or poisoning gauge point-in-time signals, the per-component `authz_scope` helper invocation against `cpt-cf-usage-collector-contract-authz-resolver` inside `cpt-cf-usage-collector-component-ingestion-gateway` makes every emit fail-closed on attribution authorization, UsageType existence and `metadata_fields` are resolved deterministically per record through a `get_usage_type` SPI dispatch against the plugin's `usage_type_catalog`, and persistence is delegated through the contract-stable Plugin SPI so the metering substrate keeps a single, narrow ingestion contract regardless of the operator-selected storage backend.

**Requirements**: `cpt-cf-usage-collector-fr-ingestion`, `cpt-cf-usage-collector-fr-idempotency`, `cpt-cf-usage-collector-fr-record-metadata`, `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`, `cpt-cf-usage-collector-fr-counter-semantics`, `cpt-cf-usage-collector-fr-gauge-semantics`, `cpt-cf-usage-collector-fr-tenant-attribution`, `cpt-cf-usage-collector-fr-resource-attribution`, `cpt-cf-usage-collector-fr-subject-attribution`, `cpt-cf-usage-collector-fr-ingestion-authorization`, `cpt-cf-usage-collector-fr-usage-compensation`, `cpt-cf-usage-collector-nfr-throughput`, `cpt-cf-usage-collector-nfr-throughput-profile`, `cpt-cf-usage-collector-nfr-ingestion-latency`, `cpt-cf-usage-collector-nfr-workload-isolation`

**Principles**: `cpt-cf-usage-collector-principle-idempotency-by-key`, `cpt-cf-usage-collector-principle-semantics-enforcement`, `cpt-cf-usage-collector-principle-fail-closed`, `cpt-cf-usage-collector-principle-pluggable-storage`

### 1.3 Actors

| Actor                                             | Role in Feature                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cpt-cf-usage-collector-actor-usage-source`       | Authenticated caller that emits usage records (single or batched, REST or SDK) through `POST /usage-collector/v1/records`; supplies the full attribution tuple (target tenant via `SecurityContext`, mandatory `ResourceRef`, optional `SubjectRef`, UsageType `gts_id`) and a mandatory caller-supplied idempotency key per record; subject to PDP authorization on the full tuple (the calling-gear identity is read by the PDP directly from `SecurityContext`), to semantics-dependent invariants / `cpt-cf-usage-collector-fr-gauge-semantics`, and to the fixed 8 KiB `RecordMetadata` size cap |
| `cpt-cf-usage-collector-actor-platform-developer` | Integrates callers with the Usage Collector via the in-process SDK trait (`emit` / `emit_batch` operations) routed through the same Ingestion Gateway as the REST surface; consumes the published Plugin SPI documentation when authoring a storage backend that persists `usage_records` under the composite dedup key `(tenant_id, gts_id, idempotency_key)`; the SDK trait deliberately excludes UsageType catalog management per `sdk-trait.md` §Out of scope, so UsageType existence/semantics discovery dispatches `get_usage_type` directly against `cpt-cf-usage-collector-contract-storage-plugin` per record rather than through a separate SDK call                 |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) -- Usage Record Ingestion §5.1, Idempotent Ingestion §5.1, Per-Record Extensible Metadata §5.1, Counter UsageType §5.2, Gauge UsageType §5.2, Tenant Attribution §5.3, Resource Attribution §5.3, Subject Attribution §5.3, Ingestion Authorization §5.3, Data Quality Preservation §5.8, Usage Compensation §5.4 (counter value-reversal on the unified ingestion path; `cpt-cf-usage-collector-fr-usage-compensation`), Ingestion Throughput §6.1, Throughput Profile §6.1, Ingestion Latency §6.1, Workload Isolation §6.1, Batch and Report Timing §6.1, Availability Boundary §6.1, Actor catalog §2 (Usage Source, Platform Developer)
- **Design**: [DESIGN.md](../DESIGN.md) -- Ingestion Gateway component (§3.2), Emit Usage Record sequence `cpt-cf-usage-collector-seq-emit-usage` (§3.6), the SPI dedup composite (`plugin-spi.md` §"Cross-entity invariants honored by the Plugin SPI"), Unified ingestion request shape with optional `corrects_id` + signed `value` (§3.3 — single emit path; no dedicated compensate endpoint), Correction posture two-primitive taxonomy (ADR-0005 deactivation + ADR-0008 compensation, un-policed-net note in ADR-0008), Domain Model entities `UsageRecord` / `RecordMetadata` / `ResourceRef` / `SubjectRef` / `IdempotencyKey` / `UsageType` / `SecurityContext` (§3.1), PRD→DESIGN realization rows for `fr-ingestion`, `fr-idempotency`, `fr-record-metadata`, `fr-counter-semantics`, `fr-gauge-semantics`, `fr-tenant-attribution`, `fr-resource-attribution`, `fr-subject-attribution`, `fr-ingestion-authorization`, `fr-data-quality`, `fr-usage-compensation`, `nfr-throughput`, `nfr-throughput-profile`, `nfr-ingestion-latency`, `nfr-workload-isolation`, `nfr-batch-and-report-timing`, `nfr-availability-boundary` (§5.3)
- **ADR**: [ADR/0008-usage-compensation.md](../ADR/0008-usage-compensation.md) -- `cpt-cf-usage-collector-adr-usage-compensation` — counter value-reversal as a signed-negative `UsageRecord` whose `corrects_id` is set to the referenced ordinary usage row, persisted on the unified ingestion path with PDP attribution + mandatory idempotency; complemented by [ADR/0005-monotonic-deactivation.md](../ADR/0005-monotonic-deactivation.md) (`cpt-cf-usage-collector-adr-monotonic-deactivation`) for the orthogonal cross-kind retraction primitive; [ADR/0011-consistency-contract.md](../ADR/0011-consistency-contract.md) (`cpt-cf-usage-collector-adr-consistency-contract`) — the synchronous `Acknowledged` outcome returned by this feature is the surface read-after-write caller flows MUST consume; the read-side floor and per-plugin ceiling live with `cpt-cf-usage-collector-feature-usage-query`; [ADR/0012-unified-plugin-catalog-and-gts-id-reference.md](../ADR/0012-unified-plugin-catalog-and-gts-id-reference.md) (`cpt-cf-usage-collector-adr-0012-unified-plugin-catalog-and-gts-id-reference`) — unified plugin-DB UsageType Catalog and `gts_id` reference (usage records reference usage types by `gts_id`; the in-plugin reference scheme — column type, index choice — is plugin-author choice per DESIGN §3.2 and out of FEATURE scope)
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) -- §2.3 Usage Emission
- **Foundation feature**: [foundation.md](./foundation.md) -- SecurityContext acceptance at the surface boundaries (REST `Extension<SecurityContext>` from ToolKit gateway middleware via `OperationBuilder::authenticated()`; SDK trait methods accepting `ctx: &SecurityContext` as the first parameter), PDP enforcement via the per-component `authz_scope` helper (`cpt-cf-usage-collector-flow-foundation-pdp-authorize`), plugin host binding, tenant isolation, fail-closed posture (reused, not re-defined)
- **UsageType Lifecycle feature**: [usage-type-lifecycle.md](./usage-type-lifecycle.md) -- platform-global usage-type catalog persisted in the plugin's `usage_type_catalog` table; the ingestion hot path dispatches `get_usage_type` against `cpt-cf-usage-collector-contract-storage-plugin` per record- **Plugin SPI reference**: [plugin-spi.md](../plugin-spi.md) -- usage-record persistence capability and the storage-plugin composite idempotency key `(tenant_id, gts_id, idempotency_key)`
- **SDK trait reference**: [sdk-trait.md](../sdk-trait.md) -- `emit` and `emit_batch` operations routed through the Ingestion Gateway (UsageType catalog management deliberately excluded per §Out of scope)
- **REST contract**: [usage-collector-v1.yaml](../usage-collector-v1.yaml) -- `POST /usage-collector/v1/records` path (single and batched submissions)
- **Dependencies**: `cpt-cf-usage-collector-feature-foundation`, `cpt-cf-usage-collector-feature-usage-type-lifecycle`

### 1.5 Explicit Non-Applicability

- **UX** (`UX-FDESIGN-001` user journey, `UX-FDESIGN-002` accessibility): Not applicable because the usage-emission feature is a backend write surface (`POST /usage-collector/v1/records` plus the in-process SDK `emit` / `emit_batch` operations routed through the same Ingestion Gateway); there is no human-facing UI in this gear, the only direct consumers are authenticated callers (`cpt-cf-usage-collector-actor-usage-source`) and SDK integrators (`cpt-cf-usage-collector-actor-platform-developer`), and any UI surfacing of usage data is delivered downstream by the §2.4 Usage Query consumers outside this feature's scope. Developer experience on the ingestion contract is encoded through the deterministic `Problem` error envelopes and idempotency semantics published by `usage-collector-v1.yaml` and `sdk-trait.md`.

## 2. Actor Flows (CDSL)

### Emit Record (single)

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-usage-emission-emit-record`

**Actor**: `cpt-cf-usage-collector-actor-usage-source`

> **Surface posture (single emit).** This flow describes the SDK single-emit path `UsageCollectorClientV1::create_usage_record(ctx, …)`, which returns `Result<UsageRecord, UsageCollectorError>`: any per-record validation, authorization, or SPI failure lifts to a whole-call `Err(UsageCollectorError::*)` (typed variants: `Authorization`, `UsageTypeNotFound`, `UnknownMetadataKey`, `InvalidMetadataField` / `DuplicateMetadataField`, `IdempotencyConflict`, `PluginUnavailable`, `Internal`, …) rather than an `outcome="rejected"` envelope. The REST single-emit form is wire-level a one-item batch through `POST /usage-collector/v1/records`, so its `CreateUsageRecordsResponse` envelope semantics (HTTP `200` on accept, `207 Multi-Status` if `results[0].outcome="rejected"`) live in `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`. Per-record envelope semantics apply ONLY on the REST path or the SDK batch form (`create_usage_records`).

**Success Scenarios**:

- An authenticated caller calls `UsageCollectorClientV1::create_usage_record(ctx, record)` (routed through `cpt-cf-usage-collector-component-ingestion-gateway`); `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization` resolves the `SecurityContext` and authorizes the full attribution tuple, `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` resolves the UsageType `gts_id` and `UsageType`, `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` enforces counter/gauge invariants, `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement` enforces the fixed 8 KiB `RecordMetadata` cap, and the host invokes the Plugin SPI Method 1 single-record persist capability (`create_usage_record`) under the composite key `(tenant_id, gts_id, idempotency_key)` per `plugin-spi.md` — the plugin is the single source of dedup truth (the UNIQUE `(tenant_id, gts_id, idempotency_key)` constraint atomically performs the dedup check and the persist); the trait returns `Ok(UsageRecord)` carrying the caller-supplied `uuid` and all caller-supplied canonical fields.
- An EXACT-EQUALITY retry sharing the same composite key — where ALL caller canonical fields (`value`, `ResourceRef`, optional `SubjectRef`, and `RecordMetadata`) match the stored record — returns `Ok(UsageRecord)` carrying the previously-persisted record body (**silent absorb**: the SDK surface does not distinguish a fresh insert from an exact-equality idempotency retry — the SPI returns `Ok(UsageRecord)` in both cases — and counter totals are not inflated).

**Error Scenarios**:

- The SDK trait is invoked without a `ctx` argument — disallowed at compile time by the `create_usage_record(ctx: &SecurityContext, …)` signature; the collector never synthesizes identity. (REST callers without an `Extension<SecurityContext>` are rejected upstream — that path lives in `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:inst-emit-batch-missing-ctx`.)
- PDP denies the ingestion attribution tuple — lifted as `Err(UsageCollectorError::Authorization(_))`; no SPI dispatch occurs.
- UsageType `gts_id` is not present in the plugin's `usage_type_catalog` (the per-record `get_usage_type` SPI dispatch surfaces `Err(UsageTypeNotFound { gts_id })`) — re-classified at the call site as `Err(UsageCollectorError::UsageTypeNotFound(gts_id))`; no usage-record write dispatch occurs.
- Counter record carries a negative `value` (violates the counter non-negativity invariant `value >= 0`) — once `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` lands, this will lift as `Err(UsageCollectorError::Validation(_))` carrying the `SEMANTICS_VIOLATION` reason; today the algorithm is not yet wired and counter-negative submissions reach the SPI unchecked (see `usage-emission.sync-report.md` outstanding #2).
- `RecordMetadata` violates the closed-shape rule — lifted as `Err(UsageCollectorError::UnknownMetadataKey { key })` citing the offending key; no SPI dispatch occurs.
- `RecordMetadata` exceeds the fixed 8 KiB cap — lifted as `Err(UsageCollectorError::MetadataValidationError(_))` carrying the measured size and the configured cap; no SPI dispatch occurs.
- Plugin SPI transport / readiness / persistence error (any `Err(UsageCollectorPluginError::*)` other than `IdempotencyConflict`) — lifted through `UsageCollectorPluginError → DomainError → UsageCollectorError` (e.g., `PluginUnavailable`, `Internal`); no record is acknowledged.
- A same-key submission whose canonical fields DIFFER from the stored record (e.g., the same `(tenant_id, gts_id, idempotency_key)` resubmitted with a different `value`) — the plugin returns `Err(UsageCollectorPluginError::IdempotencyConflict { idempotency_key, gts_id })`, which lifts as `Err(UsageCollectorError::IdempotencyConflict { idempotency_key, gts_id })` (AlreadyExists/409) carrying the conflicting key; the second write is NOT silently absorbed.

**Steps**:

1. [ ] - `p1` - Caller invokes `UsageCollectorClientV1::create_usage_record(ctx, record)` per `sdk-trait.md` Method 1 with `ctx: &SecurityContext` as the first parameter and a `UsageRecord` payload carrying the mandatory caller-supplied `IdempotencyKey` - `inst-emit-record-submit`
2. [ ] - `p1` - The `ctx: &SecurityContext` parameter is compile-time required by the trait signature, so a missing context is rejected at the type-system boundary rather than at runtime; the collector never synthesizes identity. (The REST one-item-batch surface routes its `Extension<SecurityContext>` check through `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch:inst-emit-batch-missing-ctx`.) - `inst-emit-record-missing-ctx`
3. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization` to authorize the attribution tuple through `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (per-component `authz_scope` helper wrapping `cpt-cf-usage-collector-contract-authz-resolver`) against the inbound `SecurityContext` and the attribution tuple (`tenant_id`, `ResourceRef`, optional `SubjectRef`, UsageType `gts_id`), receiving the (`PdpDecision`, `PdpConstraint` set) pair - `inst-emit-record-attrib-authz`
4. [ ] - `p1` - **IF** the per-record authorization outcome is `deny` lift the deny into `Err(UsageCollectorError::Authorization(_))` (carrying the propagated platform-authorization context with `reason="AUTHZ"`) and **RETURN** that `Err` — no SPI dispatch occurs - `inst-emit-record-pdp-deny`
5. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` with the record's UsageType `gts_id` to obtain the resolved `UsageType` (`kind` read from the catalog row's `kind` field) and the declared `metadata_fields` via a `get_usage_type` SPI dispatch against `cpt-cf-usage-collector-contract-storage-plugin` - `inst-emit-record-catalog-lookup`
6. [ ] - `p1` - **IF** the catalog lookup returns `not-found` lift the absence into `Err(UsageCollectorError::UsageTypeNotFound(gts_id))` and **RETURN** that `Err` without falling back to a direct Plugin SPI catalog read - `inst-emit-record-usage-type-not-found`
7. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` with the submitted value and the catalog-resolved `UsageType` (and optional `unit`) - `inst-emit-record-semantics-check`
8. [ ] - `p1` - **IF** the semantics-enforcement algorithm returns a counter-or-gauge semantics-violation violation lift the violation into `Err(UsageCollectorError::Validation(_))` carrying `context.reason="SEMANTICS_VIOLATION"`, then **RETURN** that `Err` - `inst-emit-record-semantics-invalid`
9. [ ] - `p1` - Perform the **closed-shape** metadata-key check against the usage type's `metadata_fields` (resolved per record via the `get_usage_type` SPI dispatch above): every key in the candidate `RecordMetadata` MUST be a declared member of `metadata_fields`; otherwise lift the offending key into `Err(UsageCollectorError::UnknownMetadataKey { key })` and **RETURN** that `Err` - `inst-emit-record-metadata-closed-shape`
10. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement` against the record's `RecordMetadata` payload (fixed 8 KiB cap) - `inst-emit-record-metadata-cap`
11. [ ] - `p1` - **IF** the metadata-size-cap algorithm returns `metadata-too-large` lift the measurement into `Err(UsageCollectorError::MetadataValidationError(_))` carrying the measured size and the configured cap, then **RETURN** that `Err` - `inst-emit-record-metadata-too-large`
12. [ ] - `p1` - **TRY** invoke the Plugin SPI Method 1 single-record persist capability (`create_usage_record`) under the composite key `(tenant_id, gts_id, idempotency_key)` per `plugin-spi.md` — the plugin is the single source of dedup truth (the UNIQUE `(tenant_id, gts_id, idempotency_key)` constraint atomically performs the dedup check and the persist), and the trait surfaces either `Ok(UsageRecord)` — covering **both** a fresh persist and a silent-absorb exact-equality dedup (byte-identical stored body, no second write, counter totals not inflated) — or `Err(UsageCollectorPluginError::IdempotencyConflict { idempotency_key, gts_id })` for a same-key canonical-field mismatch, or another `Err(UsageCollectorPluginError::*)` variant for SPI transport / readiness / persistence failures. The ambient `tracing::Span` / OpenTelemetry context opened by the ToolKit gateway middleware (REST) or the in-process caller (SDK) propagates across the SPI call per `plugin-spi.md` §"Trace context propagation"; trace context is ambient, not an explicit parameter - `inst-emit-record-spi-dispatch`
13. [ ] - `p1` - **CATCH** Plugin SPI transport / readiness / persistence error (any `Err` variant other than `IdempotencyConflict`) - `inst-emit-record-spi-catch`
    1. [ ] - `p1` - Lift the SPI failure through `UsageCollectorPluginError → DomainError → UsageCollectorError` (e.g., `Err(UsageCollectorError::PluginUnavailable)` for readiness, `Err(UsageCollectorError::Internal(_))` for unmapped variants) and **RETURN** that `Err`; no record is acknowledged - `inst-emit-record-spi-fail`
14. [ ] - `p1` - **IF** the SPI returned `Err(UsageCollectorPluginError::IdempotencyConflict { idempotency_key, gts_id })` (a same-key submission whose canonical fields differ from the stored record) lift it into `Err(UsageCollectorError::IdempotencyConflict { idempotency_key, gts_id })` (AlreadyExists/409) and **RETURN** that `Err` — the second write is NOT silently absorbed - `inst-emit-record-conflict`
15. [ ] - `p1` - **ELSE** the SPI returned `Ok(UsageRecord)`; **RETURN** that `Ok(UsageRecord)` with the persisted body (the caller-supplied `uuid` plus all caller-supplied canonical fields, byte-identical on a fresh insert, byte-identical to the previously persisted row on a silent-absorb exact-equality retry) - `inst-emit-record-accepted`

### Emit Records Batch

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Actor**: `cpt-cf-usage-collector-actor-usage-source`

**Success Scenarios**:

- An authenticated caller submits a batch of up to 100 usage records via `POST /usage-collector/v1/records` (`CreateUsageRecordsRequest.records` with `maxItems: 100` per the wire-level cap declared in `usage-collector-v1.yaml`) or via the SDK batch-emit operation routed through `cpt-cf-usage-collector-component-ingestion-gateway`; `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization` authorizes each record's attribution tuple, `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` resolves each record's `gts_id` via a per-record `get_usage_type` SPI dispatch against `cpt-cf-usage-collector-contract-storage-plugin`, `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` and `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement` validate each record independently, and the host invokes the Plugin SPI Method 2 batch persist capability (`create_usage_records`) under the per-record composite key `(tenant_id, gts_id, idempotency_key)` per `plugin-spi.md` — the plugin is the single source of dedup truth (the UNIQUE `(tenant_id, gts_id, idempotency_key)` constraint atomically performs the dedup check and the persist per record) and returns a per-record `Result<UsageRecord, UsageCollectorPluginError>` list in input order; per-record outcomes are surfaced in input order (`outcome="accepted"` for both a fresh persist and a silent-absorb exact-equality retry, or `outcome="rejected"` carrying a per-record `error: Problem` for any per-record failure including `IDEMPOTENCY_CONFLICT`) per `usage-collector-v1.yaml`.
- A mixed-outcome batch (at least one rejected plus at least one accepted) returns HTTP `207 Multi-Status` carrying the per-record outcome array in input order; per-record validation failures, PDP denials per record, unknown UsageTypes, semantics violation violations, metadata oversize, same-key canonical-field mismatches (`context.reason="IDEMPOTENCY_CONFLICT"`), and per-record SPI errors are all surfaced as `Rejected` entries inside the same batch result — there is no whole-batch rollback per-record acceptance promise.

**Error Scenarios**:

- Batch size exceeds the per-call cap of 100 records (wire-level enforcement via the `maxItems: 100` constraint on `CreateUsageRecordsRequest.records` in `usage-collector-v1.yaml`) — request-level structural validation rejection with the `Problem` envelope (HTTP `400`) per `usage-collector-v1.yaml` before any per-record processing.
- Empty `records` list (violates the `minItems: 1` schema constraint) — request-level structural validation rejection with the `Problem` envelope (HTTP `400`) per `usage-collector-v1.yaml`.
- Request arrives without a resolved `SecurityContext` (REST handler did not receive `Extension<SecurityContext>` from ToolKit gateway middleware, or the SDK trait was invoked without a `ctx` argument) — request-level rejection via the canonical `Unauthenticated` `Problem` envelope per `usage-collector-v1.yaml`; the collector never synthesizes identity and no per-record processing occurs.
- PDP denies every record in the batch (e.g., the calling gear is unauthorized for the requested tenant in aggregate) — the per-record outcome array surfaces a `rejected` entry with `context.reason="AUTHZ"` for every record; whole-batch HTTP `207 Multi-Status` is returned per `usage-collector-v1.yaml` because PDP is a per-tuple decision under `cpt-cf-usage-collector-flow-foundation-pdp-authorize` and there is no envelope-level PDP deny mode.
- Per-record errors (unknown UsageType, semantics violation violation, metadata oversize, same-key canonical-field mismatch with `context.reason="IDEMPOTENCY_CONFLICT"`, per-record SPI failure) — surfaced per record inside the result list while the request itself returns HTTP `200` (all accepted) or HTTP `207` (mixed or all-rejected — i.e., whenever ≥1 record is rejected, single-record conflict included), per `usage-collector-v1.yaml`.

**Steps**:

1. [ ] - `p1` - Caller submits an `CreateUsageRecordsRequest` of up to 100 `UsageRecord` payloads — on REST through `POST /usage-collector/v1/records`; the REST handler receives `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`) and W3C audit-correlation headers — or on the SDK through `UsageCollectorClientV1::create_usage_records(ctx, ...)` with `ctx: &SecurityContext` as the first parameter per `sdk-trait.md` Method 2; the payload carries one mandatory caller-supplied `IdempotencyKey` per record - `inst-emit-batch-submit`
2. [ ] - `p1` - **IF** the REST handler receives no `Extension<SecurityContext>` (gateway middleware rejected the call upstream) or the SDK trait is invoked without a `ctx` argument **RETURN** the canonical `Unauthenticated` `Problem` envelope per `usage-collector-v1.yaml` default response; the collector never synthesizes identity - `inst-emit-batch-missing-ctx`
3. [ ] - `p1` - **IF** the request `records` array is empty (`minItems: 1` violation) or larger than 100 entries (`maxItems: 100` violation per the wire-level cap declared in `usage-collector-v1.yaml`) **RETURN** the request-level structural validation `Problem` envelope (HTTP `400`) per `usage-collector-v1.yaml` without any per-record processing - `inst-emit-batch-cap-check`
4. [ ] - `p1` - PDP authorization runs per-record inline inside the foreach validation loop at step 5 (see `inst-emit-batch-record-pdp`); there is no upfront batch-PDP pre-pass — `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization` is invoked once per record at the point where its outcome is consumed, preserving the per-tuple PDP contract (one PDP call per record's caller-supplied attribution tuple via the per-component `authz_scope` helper wrapping `cpt-cf-usage-collector-contract-authz-resolver`) - `inst-emit-batch-pdp`
5. [ ] - `p1` - **FOR EACH** `UsageRecord` in the request `records` array (in input order, preserving index for the per-record result), consume its `allow`/`deny` outcome from step 4 - `inst-emit-batch-foreach-validate`
   1. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization` for this record via the per-component `authz_scope` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) with the inbound `SecurityContext` and the record's attribution tuple (`tenant_id`, `ResourceRef`, optional `SubjectRef`, UsageType `gts_id`), receiving an `allow`/`deny` outcome - `inst-emit-batch-record-pdp`
   2. [ ] - `p1` - **IF** the per-record PDP outcome is `deny` record the per-record outcome `rejected` with the propagated platform-authorization envelope (`context.reason="AUTHZ"`) and CONTINUE to the next record - `inst-emit-batch-record-deny`
   3. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` with the record's UsageType `gts_id` - `inst-emit-batch-record-catalog`
   4. [ ] - `p1` - **IF** the catalog lookup returned `not-found` record the per-record outcome `rejected` (`context.reason="UNKNOWN_USAGE_TYPE"`) and CONTINUE - `inst-emit-batch-record-unknown-usage-type`
   5. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` with the catalog-resolved `UsageType` (and optional `unit`) - `inst-emit-batch-record-semantics`
   6. [ ] - `p1` - **IF** the semantics-enforcement algorithm returned any `invalid-*` outcome record the per-record outcome `rejected` with the appropriate `context.reason` per the algorithm-outcome → wire-reason-code mapping defined under `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` (`SEMANTICS_VIOLATION` for matrix-cell value-sign violations including `invalid-counter-delta`, `invalid-compensation-value`, and the defensive `unsupported-kind`; `GAUGE_COMPENSATION_REJECTED` for `invalid-gauge-compensation`; `CORRECTS_ID_NOT_FOUND` / `CORRECTS_ID_TARGETS_COMPENSATION` / `CORRECTS_ID_WRONG_SCOPE` / `CORRECTS_ID_INACTIVE` for the four L1 referential failures) and CONTINUE - `inst-emit-batch-record-semantics-invalid`
   7. [ ] - `p1` - Perform the **closed-shape** metadata-key check against the usage type's `metadata_fields` (resolved via the catalog lookup at step 5.3); **IF** any key in the candidate `RecordMetadata` is not a declared member of `metadata_fields` record the per-record outcome `rejected` (`context.reason="UNKNOWN_METADATA_KEY"`, citing `context.key` and `instance_path="/metadata/{key}"`) and CONTINUE - `inst-emit-batch-record-metadata-closed-shape`
   8. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement` against the record's `RecordMetadata` - `inst-emit-batch-record-metadata`
   9. [ ] - `p1` - **IF** the metadata-size-cap algorithm returned `metadata-too-large` lift the measurement into `UsageCollectorError::MetadataValidationError(_)` (carrying the measured size and the configured cap) and record the per-record outcome `rejected` with the per-record `error: Problem` lifted from that variant (`context.reason="METADATA_VALIDATION"`, field violation on `metadata_fields`), then CONTINUE - `inst-emit-batch-record-metadata-too-large`
   10. [ ] - `p1` - Mark the record as eligible-for-persist and append it to the batch dispatch buffer in input-order index - `inst-emit-batch-record-eligible`
6. [ ] - `p1` - **TRY** invoke the Plugin SPI Method 2 batch persist capability (`create_usage_records`) under the per-record composite key `(tenant_id, gts_id, idempotency_key)` — the single SPI call drives the plugin's native bulk-write path and returns a per-record `Result<UsageRecord, UsageCollectorPluginError>` list in input order per `plugin-spi.md` Method 2; the plugin is the single source of dedup truth (the UNIQUE constraint atomically performs the dedup check and the persist per record). The ambient `tracing::Span` / OpenTelemetry context opened upstream propagates across the SPI call per `plugin-spi.md` §"Trace context propagation" - `inst-emit-batch-spi-dispatch`
7. [ ] - `p1` - **CATCH** outer call-level Plugin SPI failure (the whole `create_usage_records` invocation returned `Err(UsageCollectorPluginError::*)` — e.g. plugin handle resolution failure, host-contract breach surfaced as `BackendError { kind: "contract_violation", … }`, transport collapse before any per-record result was produced) - `inst-emit-batch-spi-catch`
   1. [ ] - `p1` - Surface the failure as a **whole-request** canonical `Problem` envelope (HTTP `503`/`504` per the usage-collector-v1.yaml availability mapping); the failure would hit every eligible-for-persist record identically, so it is reported once at the request level rather than fanned out per-record - `inst-emit-batch-spi-fail-mark`
8. [ ] - `p1` - **FOR EACH** SPI per-record result paired with its original input index - `inst-emit-batch-foreach-spi`
   1. [ ] - `p1` - **IF** the per-record result is `Ok(UsageRecord)` record the per-record outcome `accepted` carrying the persisted body (caller-supplied `uuid` plus all canonical fields; covers both a fresh insert and a silent-absorb exact-equality retry) - `inst-emit-batch-record-accepted`
   2. [ ] - `p1` - **ELSE IF** the per-record result is `Err(UsageCollectorPluginError::IdempotencyConflict { idempotency_key, gts_id })` (a same-key submission whose canonical fields differ from the stored record) record the per-record outcome `rejected` with a per-record `error: Problem` (`context.reason="IDEMPOTENCY_CONFLICT"`, AlreadyExists/409) carrying the conflicting `idempotency_key` and `gts_id`; the second write is NOT silently absorbed - `inst-emit-batch-record-conflict`
   3. [ ] - `p1` - **ELSE** the per-record result is some other `Err(UsageCollectorPluginError::*)` (per-record `PluginUnavailable`, `Timeout`, `BackendError`); record the per-record outcome `rejected` with a per-record `error: Problem` (`context.reason="PLUGIN_READINESS"`) - `inst-emit-batch-record-spi-err`
9. [ ] - `p1` - Compose the `CreateUsageRecordsResponse` `results` array in input-order index, merging the validation-stage outcomes from step 5 with the SPI-stage outcomes from step 8 - `inst-emit-batch-compose-response`
10. [ ] - `p1` - **IF** every per-record outcome is `accepted` **RETURN** HTTP `200` with the `CreateUsageRecordsResponse` per `usage-collector-v1.yaml` - `inst-emit-batch-return-200`
11. [ ] - `p1` - **ELSE** **RETURN** HTTP `207 Multi-Status` with the `CreateUsageRecordsResponse` carrying the mixed per-record outcomes in input order (no whole-batch rollback per-record acceptance promise) - `inst-emit-batch-return-207`

### Compensation Emission

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-usage-emission-compensation`

**Actor**: `cpt-cf-usage-collector-actor-usage-source`

**Trigger** (real-world give-back from the caller): a capacity refund, a partial cancellation, a dispute resolution, a billing-period correction, or any other caller-determined value-reversal event. The trigger is owned by the caller — UC records the reversal the caller decides to apply, never computes one itself.

**Success Scenarios**:

- An authenticated caller submits a counter-only value-reversal record on the **same unified ingestion path** (`POST /usage-collector/v1/records` with a one-item or batched `CreateUsageRecordsRequest` whose record carries `value < 0` and a non-empty `corrects_id` pointing at a previously emitted ordinary usage row (one with `corrects_id IS NULL`); or the equivalent SDK emit operation routed through `cpt-cf-usage-collector-component-ingestion-gateway`) — there is NO dedicated `compensate` REST path, SDK method, or Plugin SPI call and DESIGN §3.3. `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization` performs PDP attribution on the same per-record tuple as ordinary ingestion, `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` resolves the UsageType `gts_id` and `UsageType`, `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` enforces the four-cell `(MetricSemantics × corrects_id presence)` value matrix and the L1 `corrects_id` referential checks (the referenced row exists, has `corrects_id IS NULL`, shares `(tenant_id, gts_id)`, and is `status=active`), `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement` enforces the `RecordMetadata` cap, and the host invokes the Plugin SPI persist capability (`create_usage_record` for single, `create_usage_records` for batch) under the same composite key `(tenant_id, gts_id, idempotency_key)` per `plugin-spi.md` — there is no separate compensation SPI path. The per-record acknowledgement returns `outcome="accepted"` with the caller-supplied `uuid`.
- An EXACT-EQUALITY retry of the same compensation submission (same composite `(tenant_id, gts_id, idempotency_key)` and identical canonical fields including `value`, `corrects_id`, `ResourceRef`, optional `SubjectRef`, and `RecordMetadata`) returns `outcome="accepted"` carrying the previously-persisted compensation row body (silent absorb — the wire surface does not distinguish a fresh compensation insert from an exact-equality retry; no second write occurs and no second refund effect is recorded). Mandatory idempotency prevents double-refund for free.

**Error Scenarios**:

- Request arrives without a resolved `SecurityContext` — whole-request rejection via the canonical `Unauthenticated` `Problem` envelope per `usage-collector-v1.yaml`; the collector never synthesizes identity.
- PDP denies the ingestion attribution tuple — surfaced as the per-record `outcome="rejected"` with `context.reason="AUTHZ"` (same path as ordinary ingestion; no parallel authorization surface).
- UsageType `gts_id` is not present in the plugin's `usage_type_catalog` — surfaced as the per-record `outcome="rejected"` with `context.reason="UNKNOWN_USAGE_TYPE"`.
- The target UsageType is `gauge` (gauges have no `SUM` semantics) — surfaced as the per-record `outcome="rejected"` with `context.reason="GAUGE_COMPENSATION_REJECTED"` per the four-cell value matrix (`gauge + corrects_id SET → REJECTED`) and `usage-collector-v1.yaml` Problem.context.reason taxonomy (HTTP `422`).
- A counter compensation submission (`corrects_id` set) with `value >= 0` (compensation MUST be strictly negative per `counter + corrects_id SET → value < 0`) — surfaced as the per-record `outcome="rejected"` with `context.reason="SEMANTICS_VIOLATION"` (matrix-cell value-sign violation).
- `corrects_id` refers to a row that does not exist — surfaced as the per-record `outcome="rejected"` with `context.reason="CORRECTS_ID_NOT_FOUND"` (HTTP `404`) per `usage-collector-v1.yaml` and `sdk-trait.md` `CorrectsIdNotFound`.
- `corrects_id` refers to a row that is itself a compensation (i.e. has `corrects_id IS NOT NULL`; compensating a compensation is a non-goal) — surfaced as the per-record `outcome="rejected"` with `context.reason="CORRECTS_ID_TARGETS_COMPENSATION"` (HTTP `409`) per `usage-collector-v1.yaml` and `sdk-trait.md` `CorrectsIdTargetsCompensation`.
- `corrects_id` refers to a row whose full identity tuple `(tenant_id, gts_id, resource_ref, subject_ref)` does not match the incoming compensation (cross-tenant, cross-UsageType, cross-resource, cross-subject, or a `subject_ref` presence mismatch) — surfaced as the per-record `outcome="rejected"` with `context.reason="CORRECTS_ID_WRONG_SCOPE"` (HTTP `409`) per `usage-collector-v1.yaml` and `sdk-trait.md` `CorrectsIdWrongScope`.
- `corrects_id` refers to a row whose `status != active` (deactivated, including a row **concurrently being deactivated** — the L1 "must be active" check serialises against the cascade transition) — surfaced as the per-record `outcome="rejected"` with `context.reason="CORRECTS_ID_INACTIVE"` (HTTP `409`) per `usage-collector-v1.yaml` and `sdk-trait.md` `CorrectsIdInactive`. There is no quarantine, no retry queue, and no compensating cascade for the rejection — the caller retries at its own discretion (idempotency key makes retries safe).
- Missing idempotency key — surfaced as the per-record `outcome="rejected"` (the wire-level `idempotency_key` requirement applies uniformly to ordinary usage rows and to compensation rows alike).
- A same-key submission whose canonical fields differ from the stored compensation row (including a `corrects_id` mismatch, a `value` mismatch, or a metadata-only difference) — surfaced as the per-record `outcome="rejected"` with `context.reason="IDEMPOTENCY_CONFLICT"` (AlreadyExists/409) carrying the existing record's `id`; the second write is NOT silently absorbed.

**Steps**:

1. [ ] - `p1` - Caller computes a give-back amount according to its own business logic and constructs a single or batched `UsageRecord` payload with a signed-negative `value` and a non-empty `corrects_id` pointing at the target ordinary usage row (one with `corrects_id IS NULL`), plus a mandatory caller-supplied `IdempotencyKey` — submitted on the **same unified ingestion path** as ordinary usage emission (REST `POST /usage-collector/v1/records` or the SDK emit operation) - `inst-compensation-submit`
2. [ ] - `p1` - **IF** the REST handler receives no `Extension<SecurityContext>` or the SDK trait is invoked without a `ctx` argument **RETURN** the canonical `Unauthenticated` `Problem` envelope per `usage-collector-v1.yaml`; the collector never synthesizes identity - `inst-compensation-missing-ctx`
3. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization` to authorize the per-record attribution tuple via `cpt-cf-usage-collector-flow-foundation-pdp-authorize` — same PDP surface, same per-component `authz_scope` helper, same per-record `allow`/`deny` outcome shape as ordinary ingestion - `inst-compensation-attrib-authz`
4. [ ] - `p1` - **IF** the per-record authorization outcome is `deny` record `outcome="rejected"` with `context.reason="AUTHZ"` and **RETURN** the `CreateUsageRecordsResponse` — no SPI dispatch occurs - `inst-compensation-pdp-deny`
5. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` with the record's UsageType `gts_id` to obtain the (`UsageType`, optional `unit`) pair - `inst-compensation-catalog-lookup`
6. [ ] - `p1` - **IF** the catalog lookup returns `not-found` record `outcome="rejected"` with `context.reason="UNKNOWN_USAGE_TYPE"` and **RETURN** the response - `inst-compensation-usage-type-not-found`
7. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` with the submitted (`value`, `corrects_id`, `tenant_id`, `gts_id`, `resource_ref`, optional `subject_ref`) tuple and the catalog-resolved `UsageType`; the algorithm runs the full four-cell `(MetricSemantics × corrects_id presence)` value matrix AND, when `corrects_id` is set, the L1 referential checks (existence, referenced row has `corrects_id IS NULL`, same `(tenant_id, gts_id, resource_ref, subject_ref)` identity tuple, `status=active`) - `inst-compensation-validate`
8. [ ] - `p1` - **IF** the algorithm returns any `invalid-*` outcome record `outcome="rejected"` with the appropriate `context.reason` per the algorithm-outcome → wire-reason-code mapping defined under `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` (`GAUGE_COMPENSATION_REJECTED` for `invalid-gauge-compensation`; `SEMANTICS_VIOLATION` for `invalid-counter-delta`, `invalid-compensation-value`, or the defensive `unsupported-kind`; `CORRECTS_ID_NOT_FOUND` for `invalid-corrects-id-not-found`; `CORRECTS_ID_TARGETS_COMPENSATION` for `invalid-corrects-id-targets-compensation`; `CORRECTS_ID_WRONG_SCOPE` for `invalid-corrects-id-cross-scope`; `CORRECTS_ID_INACTIVE` for `invalid-corrects-id-inactive` — which includes the "referenced record must be active" branch that handles concurrent deactivation) and **RETURN** the response — no SPI dispatch occurs - `inst-compensation-validate-fail`
9. [ ] - `p1` - Perform the **closed-shape** metadata-key check against the usage type's `metadata_fields` (resolved at step 5): every key in the candidate `RecordMetadata` MUST be a declared member of `metadata_fields`; otherwise record `outcome="rejected"` with `context.reason="UNKNOWN_METADATA_KEY"`, `context.key`, and `instance_path="/metadata/{key}"`, and **RETURN** the response - `inst-compensation-metadata-closed-shape`
10. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement` against the record's `RecordMetadata` payload - `inst-compensation-metadata-cap`
11. [ ] - `p1` - **IF** the metadata-size-cap algorithm returns `metadata-too-large` record `outcome="rejected"` with `context.reason="METADATA_SIZE"` and **RETURN** the response - `inst-compensation-metadata-too-large`
12. [ ] - `p1` - **TRY** invoke the Plugin SPI persist capability (`create_usage_record` for single, `create_usage_records` for batch) under the same composite key `(tenant_id, gts_id, idempotency_key)` as ordinary ingestion per `plugin-spi.md`; the SPI receives the row with a signed-negative `value` and the `corrects_id` pointer to the referenced ordinary usage row (one with `corrects_id IS NULL`), and persists it with `status=active` - `inst-compensation-spi-dispatch`
13. [ ] - `p1` - **CATCH** Plugin SPI transport / readiness / persistence error - `inst-compensation-spi-catch`
    1. [ ] - `p1` - Compose `CreateUsageRecordsResponse` with `outcome="rejected"` and `context.reason="PLUGIN_READINESS"` while preserving the audit-correlation context, then **RETURN** that response; no record is acknowledged - `inst-compensation-spi-fail`
14. [ ] - `p1` - **IF** the SPI returned `Err(UsageCollectorPluginError::IdempotencyConflict { idempotency_key, gts_id })` (a same-key submission whose canonical fields differ from the stored record) record `outcome="rejected"` with `context.reason="IDEMPOTENCY_CONFLICT"` (AlreadyExists/409) carrying the conflicting `idempotency_key` and `gts_id`, and **RETURN** the response — the second write is NOT silently absorbed - `inst-compensation-conflict`
15. [ ] - `p1` - **ELSE** the SPI returned `Ok(UsageRecord)` (a fresh compensation insert, or a silent-absorb exact-equality retry — the wire surface does not distinguish the two); record `outcome="accepted"` carrying the persisted compensation row body (caller-supplied `uuid` plus all canonical fields, byte-identical to the request or to the previously-persisted row), propagate the audit-correlation context, then **RETURN** the response - `inst-compensation-accepted`

## 3. Processes / Business Logic (CDSL)

### Attribution and PDP Authorization

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`

**Input**: an inbound `POST /usage-collector/v1/records` REST request carrying the gateway-resolved `Extension<SecurityContext>`, audit-correlation headers, and a per-record attribution tuple lifted directly off each `UsageRecord` payload — caller-supplied `tenant_id` (never derived from the inbound `SecurityContext`), `ResourceRef`, optional `SubjectRef` (its mandatory `subject_id` plus the optional `subject_type` qualifier when populated), UsageType `gts_id`; OR an SDK `UsageCollectorClientV1::create_usage_record(ctx, ...)` / `create_usage_records(ctx, ...)` invocation carrying `ctx: &SecurityContext` as the first parameter and an equivalent attribution tuple lifted off each `UsageRecord`.

**Output**: A per-record outcome list (each entry is `allow` or `deny` with the propagated platform-authorization envelope on deny — `context.reason="AUTHZ"`). PDP decisions are per-attribution-tuple — there is no envelope-level PDP `deny` aggregation. This algorithm MUST NOT re-implement PDP logic — it invokes the per-component `authz_scope` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) once per record and forwards the attribution tuple to the PDP. Authentication is owned by the ToolKit gateway upstream of the REST handler and by the in-process caller on the SDK trait surface; the collector NEVER synthesizes identity and NEVER consults an authentication contract.

> **Constraint propagation (deferred).** The PDP returns an `AccessScope` carrying the `(PdpDecision, PdpConstraint set)` pair on every permit. The ingestion algorithm CURRENTLY discards the scope after the allow/deny decision: no constraint is plumbed to the persist path, because the foundation Plugin SPI does not accept per-record PDP constraints and there is no row-level filtering on the write path that could consume them. When the read-side query feature lands (or when a write-time row-level policy is introduced), this algorithm will be revisited to thread the `AccessScope` through to the SPI; until then, the carried-through pair is intentionally `()`.

**Steps**:

1. [ ] - `p1` - Receive the inbound `SecurityContext` at the `cpt-cf-usage-collector-component-ingestion-gateway` boundary — on REST as `Extension<SecurityContext>` from the gateway middleware, on SDK as the `ctx: &SecurityContext` first argument — and extract the per-record attribution tuples from the request payload - `inst-algo-attrib-receive-ctx`
2. [ ] - `p1` - **FOR EACH** per-record attribution tuple in the request envelope - `inst-algo-attrib-foreach`
   1. [ ] - `p1` - Compose the per-record attribution tuple required by `cpt-cf-usage-collector-flow-foundation-pdp-authorize` directly from the `UsageRecord` fields: the caller-supplied `tenant_id` (carried as the PDP `OWNER_TENANT_ID` attribute), `ResourceRef` (`resource_type` / `resource_id`), optional `SubjectRef` (`subject_id` carried as `OWNER_ID`, plus the optional `subject_type` qualifier carried as the `SUBJECT_TYPE` attribute only when populated), and UsageType `gts_id` - `inst-algo-attrib-compose-tuple`
   2. [ ] - `p1` - Invoke `cpt-cf-usage-collector-flow-foundation-pdp-authorize` via the per-component `authz_scope` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) with the inbound `SecurityContext` and the composed attribution tuple - `inst-algo-attrib-pdp-call`
   3. [ ] - `p1` - **IF** the foundation PDP flow returns `deny` record the per-record outcome `deny` with the propagated platform-authorization envelope (`context.reason="AUTHZ"`) - `inst-algo-attrib-pdp-deny`
   4. [ ] - `p1` - **ELSE** record the per-record outcome `allow`; the PDP-returned `AccessScope` (decision + constraint set) is intentionally discarded — see "Constraint propagation (deferred)" above — and is not threaded into the record's downstream context on the ingestion path - `inst-algo-attrib-pdp-allow`
3. [ ] - `p1` - **RETURN** the per-record outcome list to the calling flow without caching the PDP decision; PDP decisions are per-attribution-tuple, so the calling flow surfaces every `deny` outcome as a per-record `outcome="rejected"` (`context.reason="AUTHZ"`) — there is no envelope-level PDP `deny` aggregation - `inst-algo-attrib-return`

### Semantics Enforcement on Ingest

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`

**Input**: A single submitted `UsageRecord` payload (`value`, optional `corrects_id`, `tenant_id`, `gts_id`, `resource_ref`, optional `subject_ref`) plus the catalog-resolved (`UsageType`, optional `unit`) returned by `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup`.

**Output**: `valid` when the submitted record satisfies the semantics-specific invariants for its (`UsageType`, `corrects_id` presence) cell of the four-cell value matrix AND — when `corrects_id` is set — the L1 referential checks pass; or one of `invalid-counter-delta` / `invalid-gauge-compensation` / `invalid-compensation-value` / `invalid-corrects-id-not-found` / `invalid-corrects-id-targets-compensation` / `invalid-corrects-id-cross-scope` / `invalid-corrects-id-inactive` / `unsupported-kind`. This algorithm MUST encode the locked four-cell `(MetricSemantics × corrects_id presence)` value matrix verbatim, MUST enforce the L1 `corrects_id` rule synchronously on the ingestion path, and MUST NOT dispatch the Plugin SPI persist path or maintain any L2 remaining-amount tracking. Recording a caller-supplied signed-negative value when `corrects_id` is set is recording, not computing — this algorithm validates and rejects; it does NOT compute refunds, credits, credit-notes, or quota. The concurrency rule against in-flight deactivation is realized by the L1 "referenced record MUST be active" check — a compensation arriving while the referenced row is being deactivated is rejected without quarantine or retry queue.

**Four-cell value matrix** (verbatim — every step of this algorithm respects it):

| MetricSemantics | `corrects_id` | Allowed `value`                 |
| ---------- | ------------- | ------------------------------- |
| `counter`  | `IS NULL`     | `value >= 0` (unchanged)        |
| `counter`  | `SET`         | `value < 0` (strictly negative) |
| `gauge`    | `IS NULL`     | Any signed value (unchanged)    |
| `gauge`    | `SET`         | REJECTED before persistence     |

**Steps**:

1. [ ] - `p1` - Read the submitted `value`, optional `corrects_id`, `tenant_id`, and `gts_id` from the inbound `UsageRecord` payload without any Plugin SPI persist dispatch - `inst-algo-semantics-read-input-v2`
2. [ ] - `p1` - Read the catalog-resolved `UsageType` (and optional `unit`) supplied by `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` - `inst-algo-semantics-read-type-v2`
3. [ ] - `p1` - **IF** `UsageType` is `counter` - `inst-algo-semantics-counter-branch-v2`
   1. [ ] - `p1` - **IF** `corrects_id IS NULL` (ordinary usage row) - `inst-algo-semantics-counter-usage`
      1. [ ] - `p1` - **IF** the submitted `value` is below zero **RETURN** `invalid-counter-delta` and the four-cell matrix (`counter` + `corrects_id IS NULL` requires `value >= 0`) - `inst-algo-semantics-counter-usage-negative`
      2. [ ] - `p1` - **RETURN** `valid` — the `counter` + `corrects_id IS NULL` cell holds (`value >= 0`) - `inst-algo-semantics-counter-usage-valid`
   2. [ ] - `p1` - **ELSE** `corrects_id` is set (counter compensation row) - `inst-algo-semantics-counter-compensation`
      1. [ ] - `p1` - **IF** the submitted `value` is greater than or equal to zero **RETURN** `invalid-compensation-value` per the four-cell matrix (`counter` + `corrects_id SET` requires `value < 0`; zero is not accepted as a no-op compensation) and `cpt-cf-usage-collector-fr-usage-compensation` - `inst-algo-semantics-counter-compensation-non-negative`
      2. [ ] - `p1` - Perform the L1 `corrects_id` referential lookup against the storage plugin's `usage_records` projection: read the referenced row by `corrects_id` (single ingestion-time read; idempotent; not a persist dispatch) - `inst-algo-semantics-l1-lookup`
      3. [ ] - `p1` - **IF** the lookup returns `not-found` **RETURN** `invalid-corrects-id-not-found` — the referenced row MUST exist (L1 rule) - `inst-algo-semantics-l1-not-found`
      4. [ ] - `p1` - **IF** the referenced row's `corrects_id IS NOT NULL` **RETURN** `invalid-corrects-id-targets-compensation` — the referenced row MUST itself be an ordinary usage row (`corrects_id IS NULL`); compensating a compensation is a non-goal (L1 rule) - `inst-algo-semantics-l1-targets-compensation`
      5. [ ] - `p1` - **IF** the referenced row's `tenant_id != incoming.tenant_id` OR `gts_id != incoming.gts_id` OR `resource_ref != incoming.resource_ref` OR `subject_ref != incoming.subject_ref` (strict-presence equality: `None` vs `Some(_)` is a mismatch) **RETURN** `invalid-corrects-id-cross-scope` — cross-tenant, cross-usage-type, cross-resource, or cross-subject compensation is rejected per the L1 rule (a compensation MUST share the full identity tuple `(tenant_id, gts_id, resource_ref, subject_ref)` with the referenced ordinary usage row) - `inst-algo-semantics-l1-cross-scope`
      6. [ ] - `p1` - **IF** the referenced row's `status != active` **RETURN** `invalid-corrects-id-inactive` — the referenced row MUST be `active` per the L1 rule; a compensation referencing a row that is **concurrently being deactivated** is rejected by this same check (no quarantine, no retry queue, no compensating cascade for the rejection — the caller retries at its own discretion; idempotency key makes retries safe) - `inst-algo-semantics-l1-inactive-or-deactivating`
      7. [ ] - `p1` - **RETURN** `valid` — the `counter` + `corrects_id SET` cell holds (`value < 0`) and the L1 referential checks all passed; the row is recorded as-supplied (no L2 remaining-amount tracking, no refund/credit/credit-note/quota computation) - `inst-algo-semantics-counter-compensation-valid`
4. [ ] - `p1` - **ELSE IF** `UsageType` is `gauge` - `inst-algo-semantics-gauge-branch-v2`
   1. [ ] - `p1` - **IF** `corrects_id` is set **RETURN** `invalid-gauge-compensation` — `gauge` + `corrects_id SET` is rejected before persistence per the four-cell matrix (gauges have no `SUM` semantics; the only correction for a gauge is deactivation) - `inst-algo-semantics-gauge-compensation-rejected`
   2. [ ] - `p1` - Accept the submitted `value` as a point-in-time replacement and DESIGN §3.1 (gauges are stored as-is, no delta accumulation, no shape rewriting) - `inst-algo-semantics-gauge-accept-v2`
   3. [ ] - `p1` - **RETURN** `valid` — the `gauge` + `corrects_id IS NULL` cell accepts any signed value as-is - `inst-algo-semantics-gauge-valid-v2`
5. [ ] - `p1` - **ELSE** **RETURN** `unsupported-kind` (defensive — this branch is unreachable when the catalog read is consistent because `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` constrains `UsageType` to the registered enum set) - `inst-algo-semantics-unsupported-v2`

**Algorithm outcome → wire `context.reason` mapping** (verbatim contract from this algorithm to the `Problem.context.reason` taxonomy locked in `usage-collector-v1.yaml` and the SDK trait error catalog in `sdk-trait.md`):

| Algorithm outcome                                | Wire `context.reason`              | HTTP status | SDK trait variant               |
| ------------------------------------------------ | ---------------------------------- | ----------- | ------------------------------- |
| `valid`                                          | (none — accepted)                  | `200`/`207` | n/a                             |
| `invalid-counter-delta`                          | `SEMANTICS_VIOLATION`                   | `422`       | typed validation variant         |
| `invalid-compensation-value`                     | `SEMANTICS_VIOLATION`                   | `422`       | typed validation variant         |
| `invalid-gauge-compensation`                     | `GAUGE_COMPENSATION_REJECTED`      | `422`       | `GaugeCompensationRejected`     |
| `invalid-corrects-id-not-found`                  | `CORRECTS_ID_NOT_FOUND`            | `404`       | `CorrectsIdNotFound`            |
| `invalid-corrects-id-targets-compensation`       | `CORRECTS_ID_TARGETS_COMPENSATION` | `409`       | `CorrectsIdTargetsCompensation` |
| `invalid-corrects-id-cross-scope`                | `CORRECTS_ID_WRONG_SCOPE`          | `409`       | `CorrectsIdWrongScope`          |
| `invalid-corrects-id-inactive`                   | `CORRECTS_ID_INACTIVE`             | `409`       | `CorrectsIdInactive`            |
| `unsupported-kind`                               | `SEMANTICS_VIOLATION`                   | `422`       | typed validation variant         |

Notes (locked):

- The four granular `corrects_id_*` codes are sourced verbatim from `usage-collector-v1.yaml` `Problem.context.reason` enumeration (`CORRECTS_ID_NOT_FOUND`, `CORRECTS_ID_TARGETS_COMPENSATION`, `CORRECTS_ID_WRONG_SCOPE`, `CORRECTS_ID_INACTIVE`) and `sdk-trait.md` (`CorrectsIdNotFound`, `CorrectsIdTargetsCompensation`, `CorrectsIdWrongScope`, `CorrectsIdInactive`); they MUST NOT be collapsed back into a single generic code on the wire.
- `gauge` + `corrects_id SET` lifts to the dedicated `GAUGE_COMPENSATION_REJECTED` code (HTTP `422`) rather than the generic `SEMANTICS_VIOLATION` code, because the locked five-code compensation taxonomy in `usage-collector-v1.yaml` and `sdk-trait.md` carves it out as its own enum.

### Metadata Size-Cap Enforcement

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement`

**Input**: A submitted `RecordMetadata` payload (key/value map; every key MUST be a declared member of the usage type's `metadata_fields`; values are conveyed as `String` end-to-end) plus the fixed 8 KiB per record size cap. The closed-shape key-set check is performed before this algorithm; this algorithm enforces the orthogonal size cap.

**Output**: `valid` when the on-the-wire serialized size of `RecordMetadata` is at or below the fixed cap; or `metadata-too-large` carrying the measured size and the fixed cap so the caller can surface an actionable validation error. This algorithm MUST cite the fixed 8 KiB size cap and MUST NOT mutate the `RecordMetadata` payload (per `sdk-trait.md` Method 1 invariant "Persist `metadata` byte-for-byte"; SPI MUST NOT silently truncate).

**Steps**:

1. [ ] - `p1` - Receive the submitted `RecordMetadata` payload by borrowed immutable reference at the algorithm boundary so it can neither be copied nor mutated (enforced by the implementing function signature `validate_submit_record_metadata(usage_type, metadata: &serde_json::Value)`) - `inst-algo-metadata-read-input`
2. [ ] - `p1` - Use the fixed 8 KiB per-record size cap defined at compile time as the constant `RECORD_METADATA_SIZE_CAP_BYTES`; this cap is NOT operator-configurable and has no override path (matches the Output clause and G4 of the sync report) - `inst-algo-metadata-read-cap`
3. [ ] - `p1` - Serialize `RecordMetadata` to its canonical on-the-wire representation (the same representation that the Plugin SPI will persist byte-for-byte and `plugin-spi.md` Method 1 invariant 2) - `inst-algo-metadata-serialize`
4. [ ] - `p1` - Measure the serialized size in bytes - `inst-algo-metadata-measure`
5. [ ] - `p1` - **IF** the measured size exceeds the configured cap **RETURN** `metadata-too-large` carrying both the measured bytes and the configured cap bytes so the calling flow can populate the actionable validation error envelope (`context.reason="METADATA_SIZE"`) - `inst-algo-metadata-exceeds`
6. [ ] - `p1` - **ELSE** **RETURN** `valid`; the payload is forwarded unmodified to the Plugin SPI persist dispatch (`create_usage_record` / `create_usage_records`) per the byte-for-byte persistence invariant (`plugin-spi.md` Method 1 invariant 2) - `inst-algo-metadata-valid`

### Catalog Existence and Kind Lookup

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup`

**Input**: A target UsageType `gts_id` extracted from a per-record `UsageRecord` payload by `cpt-cf-usage-collector-component-ingestion-gateway`.

**Output**: A `found: true` shape descriptor carrying the resolved `UsageType` (with counter / gauge `kind` read directly from the catalog row's `UsageKind` enum via `UsageType::is_counter` / `UsageType::is_gauge`) plus the usage type's closed `metadata_fields: array<string>` (the declared metadata key set used by the ingest-time closed-shape check) when the `gts_id` is present in the plugin's `usage_type_catalog`; or `found: false` when the `gts_id` is absent (the calling flow surfaces this as the actionable not-found error envelope). This algorithm dispatches `get_usage_type` directly against `cpt-cf-usage-collector-contract-storage-plugin` on every call. The cost is one plugin SPI round-trip per record on the ingestion hot path.

**Steps**:

1. [ ] - `p1` - Read the target UsageType `gts_id` from the calling pipeline at the `cpt-cf-usage-collector-component-ingestion-gateway` boundary - `inst-algo-catalog-read-input`
2. [ ] - `p1` - Dispatch `get_usage_type(gts_id)` against `cpt-cf-usage-collector-contract-storage-plugin` - `inst-algo-catalog-spi-dispatch`
3. [ ] - `p1` - **IF** the plugin returns `Err(UsageTypeNotFound { gts_id })` **RETURN** `found: false` so the calling flow surfaces the actionable not-found error envelope - `inst-algo-catalog-not-found`
4. [ ] - `p1` - **IF** the plugin returns a transport / availability error **PROPAGATE** the platform-error envelope to the calling flow (HTTP `503` for `PluginUnavailable` / `TypesRegistryUnavailable` / `ServiceUnavailable` — plugin-side `Transient` and host-side per-call deadline expirations both lift to `ServiceUnavailable`; HTTP `500` for plugin `BackendError`, which lifts to the unclassified `Internal` envelope until a retryable-kind taxonomy is defined) - `inst-algo-catalog-spi-fail`
5. [ ] - `p1` - **ELSE** the plugin returned `Ok(UsageType { gts_id, kind, metadata_fields })`; **RETURN** `found: true` with the `kind` value and `metadata_fields` shape descriptor — the calling flow reads counter / gauge `kind` directly from the catalog row via `UsageType::is_counter` / `UsageType::is_gauge` for consumption by `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` and the ingest-time closed-shape metadata-key check - `inst-algo-catalog-found`

## 4. States (CDSL)

### Usage Record Ingestion Lifecycle State Machine

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-state-usage-emission-usage-record-ingestion-lifecycle`

**States**: `submitted`, `validated`, `persisted`, `rejected-validation`, `spi-errored`

> Note: A `deduplicated` state is **not** modeled. The Plugin SPI silently absorbs an exact-equality retry by returning `Ok(UsageRecord)` for the previously persisted row; the wire surface and this state machine collapse "fresh persist" and "silent absorb of exact-equality retry" into the single `persisted` state. The only state surfaced when an idempotency key collides with a DIFFERENT canonical-field set is `rejected-validation`, driven by the SPI's `Err(UsageCollectorPluginError::IdempotencyConflict { … })`.

**Initial State**: `submitted`

**Transitions**:

1. [ ] - `p1` - **FROM** `submitted` **TO** `validated` **WHEN** `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`, `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup`, `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`, and `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement` all return their success outcome on the per-record path (mirrors `inst-emit-record-attrib-authz`, `inst-emit-record-catalog-lookup`, `inst-emit-record-semantics-check`, `inst-emit-record-metadata-cap` in `cpt-cf-usage-collector-flow-usage-emission-emit-record` and the per-record equivalents `inst-emit-batch-record-pdp`, `inst-emit-batch-record-catalog`, `inst-emit-batch-record-semantics`, `inst-emit-batch-record-metadata` in `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`) - `inst-state-usage-record-validated`
2. [ ] - `p1` - **FROM** `submitted` **TO** `rejected-validation` **WHEN** any of the gateway-side validation algorithms returns a deterministic rejection — the inbound `SecurityContext` is missing (mirrors `inst-emit-record-missing-ctx`, `inst-emit-batch-missing-ctx`; surfaced as the canonical `Unauthenticated` `Problem` envelope), `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization` returns per-record `deny` (mirrors `inst-emit-record-pdp-deny`, `inst-emit-batch-record-deny`), `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` returns `not-found` (mirrors `inst-emit-record-usage-type-not-found`, `inst-emit-batch-record-unknown-usage-type`), `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` returns a counter-or-gauge semantics-violation violation (mirrors `inst-emit-record-semantics-invalid`, `inst-emit-batch-record-semantics-invalid`), or `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement` returns `metadata-too-large` (mirrors `inst-emit-record-metadata-too-large`, `inst-emit-batch-record-metadata-too-large`); the SPI-dispatch stage joins the same `rejected-validation` disposition when the Plugin SPI `create_usage_record` / `create_usage_records` call returns `Err(UsageCollectorPluginError::IdempotencyConflict { idempotency_key, gts_id })` for a same-key submission whose canonical fields differ (mirrors `inst-emit-record-conflict`, `inst-emit-batch-record-conflict`), surfaced as `outcome="rejected"` with `context.reason="IDEMPOTENCY_CONFLICT"` (AlreadyExists/409) carrying the conflicting `idempotency_key` and `gts_id` and NOT silently absorbed; the actionable error envelope is surfaced and no record is acknowledged - `inst-state-usage-record-rejected-validation`
3. [ ] - `p1` - **FROM** `validated` **TO** `persisted` **WHEN** the Plugin SPI `create_usage_record` / `create_usage_records` call returns `Ok(UsageRecord)` under the composite key `(tenant_id, gts_id, idempotency_key)` per `plugin-spi.md` (mirrors `inst-emit-record-accepted`, `inst-emit-batch-record-accepted`); this single transition covers **both** a fresh persist and a silent-absorb exact-equality retry — the wire surface returns `outcome="accepted"` carrying the persisted body (with the caller-supplied `uuid`) per `usage-collector-v1.yaml` and counter totals are not inflated by a retry - `inst-state-usage-record-persisted`
4. [ ] - `p1` - **FROM** `validated` **TO** `spi-errored` **WHEN** the Plugin SPI `create_usage_record` / per-record `create_usage_records` result is an `Err(UsageCollectorPluginError::*)` other than `IdempotencyConflict` (`PluginUnavailable`, `Timeout`, `BackendError`) — mirrors `inst-emit-record-spi-fail`, `inst-emit-batch-record-spi-err`; the per-record outcome is `rejected` (`context.reason="PLUGIN_READINESS"`); an outer batch-level call failure (where the whole `create_usage_records` invocation returns `Err`) surfaces as a whole-request canonical `Problem` instead per `inst-emit-batch-spi-fail-mark` - `inst-state-usage-record-spi-error`

## 5. Definitions of Done

### FR: Ingestion

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-fr-ingestion`

The system **MUST** expose `POST /usage-collector/v1/records` as the single contract-first write path for at-least-once ingestion of usage records from authenticated callers, accept a `CreateUsageRecordsRequest` carrying 1..100 `UsageRecord` payloads (single or batched) per `usage-collector-v1.yaml`, route every submission through `cpt-cf-usage-collector-component-ingestion-gateway`, and end the synchronous path with persistence through the Plugin Host's persist capability — surfacing deterministic per-record acknowledgements (`accepted` for both a fresh persist and a silent-absorb exact-equality retry, or `rejected` with a per-record `error: Problem` envelope for any per-record failure) in input order with no whole-batch rollback.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`
- `cpt-cf-usage-collector-seq-emit-usage`

**Constraints**: `cpt-cf-usage-collector-fr-ingestion`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `UsageRecord`

### FR: Idempotency

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-fr-idempotency`

The system **MUST** require a caller-supplied `IdempotencyKey` on every record and dedup retried submissions via the Plugin SPI composite key `(tenant_id, gts_id, idempotency_key)` UNIQUE constraint; the dedup check and the persist MUST be a single SPI capability (Method 1 `create_usage_record` for single, Method 2 `create_usage_records` for batch) and MUST NOT be split into separate non-transactional calls. The system **MUST** distinguish two same-key outcomes per the plugin's canonical-field equality comparison: (a) an EXACT-EQUALITY retry — where ALL caller canonical fields (`value`, `ResourceRef`, `SubjectRef`, and `RecordMetadata`) match the stored record — is dedup'd silently: the SPI returns `Ok(UsageRecord)` carrying the previously-persisted body and the wire surface returns `outcome="accepted"` indistinguishably from a fresh insert, without performing a second write and without inflating counter totals; and (b) a same-key submission with ANY canonical-field mismatch (including a metadata-only difference) MUST be rejected: the SPI returns `Err(UsageCollectorPluginError::IdempotencyConflict { idempotency_key, gts_id })` and the wire surface returns `outcome="rejected"` with a per-record `error: Problem` (`context.reason="IDEMPOTENCY_CONFLICT"`, AlreadyExists/409) — the second write MUST NOT be silently absorbed. (c) The idempotency window **MUST** be UNBOUNDED — the `IdempotencyKey` never expires, has no TTL, and the UNIQUE `(tenant_id, gts_id, idempotency_key)` constraint is permanent — and the storage plugin **MUST** preserve the `(tenant_id, gts_id, idempotency_key)` tuple permanently even when record bodies are purged/archived by retention (retention/purge MUST NOT free a dedup key) per `plugin-spi.md`.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-adr-mandatory-idempotency`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `IdempotencyKey`, `UsageRecord`

### FR: Record Metadata

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-fr-record-metadata`

The system **MUST** enforce a **closed-shape** check on the `RecordMetadata` payload at the Ingestion Gateway before any usage-record Plugin SPI write dispatch: every key in the submitted metadata MUST be a declared member of the usage type's `metadata_fields` (resolved per record via a `get_usage_type` SPI dispatch against `cpt-cf-usage-collector-contract-storage-plugin`) and every value is conveyed as `String` end-to-end; any undeclared key MUST be rejected with the `UNKNOWN_METADATA_KEY` error name (REST `context.reason="UNKNOWN_METADATA_KEY"`; SDK `UsageCollectorError::UnknownMetadataKey`) lifting to AIP-193 `InvalidArgument` / HTTP `400` carrying `context.key` and `instance_path` (e.g. `/metadata/{key}`) per `usage-collector-v1.yaml`. The system **MUST** also enforce a fixed 8 KiB per record size cap on the conforming payload at the Ingestion Gateway before any Plugin SPI dispatch, surface oversize records with the actionable validation error envelope (`context.reason="METADATA_SIZE"`) carrying the measured size and the configured cap per `usage-collector-v1.yaml`, and persist conforming payloads byte-for-byte through the Plugin SPI (no silent truncation, no rewriting, no interpretation of declared-key values) per the `sdk-trait.md` Method 1 invariant and `plugin-spi.md` Method 1 invariant 2. There is no free-form remainder and no preserved extras — undeclared keys are validation errors, not silently-stored extras.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement`

**Constraints**: `cpt-cf-usage-collector-fr-record-metadata`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `RecordMetadata`

### FR: UsageType Existence and Semantics

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-fr-usage-type-existence-and-semantics`

The system **MUST** reject any usage record whose target UsageType `gts_id` is not present in the plugin's `usage_type_catalog` at the time of submission, before any usage-record Plugin SPI write dispatch — surfaced as the per-record `outcome="rejected"` with `Problem.context.reason="UNKNOWN_USAGE_TYPE"` per `usage-collector-v1.yaml`. UsageType existence is resolved by `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup`, which dispatches `get_usage_type` against `cpt-cf-usage-collector-contract-storage-plugin` per record. The system **MUST** additionally enforce semantics-dependent invariants based on the referenced UsageType's counter / gauge classification (read from the catalog row's `kind` field via `UsageType::is_counter` / `UsageType::is_gauge`) — counter records with a negative delta `value` MUST be rejected with `Problem.context.reason="SEMANTICS_VIOLATION"`; gauge records are accepted as point-in-time values. Both rejections MUST be returned to the caller as actionable per-record `error: Problem` envelopes before any usage-record persistence, so unregistered UsageType references and semantics-violating values can never enter the `usage_records` table.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup`
- `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`

**Constraints**: `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-ingestion-gateway`
- Entities: `UsageType`, `UsageRecord`

### FR: Counter Semantics

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-fr-counter-semantics`

The system **MUST** enforce counter non-negativity at the Ingestion Gateway before any Plugin SPI dispatch — rejecting any counter-kind record whose submitted `value` is below zero — by consulting the `UsageType` and reading the row's closed `UsageKind` enum (`UsageType.kind == Counter`) through `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` and surfacing the actionable validation error envelope (`context.reason="SEMANTICS_VIOLATION"`) per `usage-collector-v1.yaml` so the counter non-negativity invariant (counter-kind records MUST have `value >= 0`) is upheld before persistence.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`

**Constraints**: `cpt-cf-usage-collector-principle-semantics-enforcement`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `UsageType`, `UsageRecord`

### FR: Gauge Semantics

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-fr-gauge-semantics`

The system **MUST** accept gauge-kind records as point-in-time values stored as-is — no delta accumulation, no rewriting, no server-side shape rewriting — per DESIGN §3.1 and `cpt-cf-usage-collector-fr-gauge-semantics` (gauge classification read from the catalog row's closed `UsageKind` enum, `UsageType.kind == Gauge`), preserving the gauge replacement semantic that the most recent accepted value supersedes prior values for the same `(tenant_id, gts_id)` pair without delta arithmetic, and rejecting any submission against a gauge UsageType that carries `corrects_id` set per the locked (`MetricSemantics` × `corrects_id` presence) value matrix in `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`

**Constraints**: `cpt-cf-usage-collector-principle-semantics-enforcement`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `UsageType`, `UsageRecord`

### FR: Tenant Attribution

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-fr-tenant-attribution`

The system **MUST** treat tenant attribution as caller-supplied in the request payload as the mandatory `tenant_id` field of `UsageRecordInput` per `usage-collector-v1.yaml` rather than server-synthesized or inferred from the inbound `SecurityContext`, include the caller-supplied `tenant_id` in the per-record attribution tuple sent to `cpt-cf-usage-collector-flow-foundation-pdp-authorize` via the per-component `authz_scope` helper inside `cpt-cf-usage-collector-component-ingestion-gateway` (which authorizes the caller's `SecurityContext` for the requested `tenant_id`), materialize the caller-supplied `tenant_id` byte-identical on every persisted record via the Plugin SPI persist capability, and refuse any ingestion attempt whose attribution tuple cannot be authorized and the PDP permit/deny outcome.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`
- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-adr-caller-supplied-attribution`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `SecurityContext`, `UsageRecord`

### FR: Resource Attribution

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-fr-resource-attribution`

The system **MUST** require a mandatory caller-supplied `ResourceRef` (composite `resource_id` plus `resource_type`) on every `UsageRecord` payload, materialize both mandatory components on every persisted record via the Plugin SPI persist capability, include the resource attribution in the per-record attribution tuple sent to `cpt-cf-usage-collector-flow-foundation-pdp-authorize`, and never synthesize resource attribution server-side.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`
- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-adr-caller-supplied-attribution`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `ResourceRef`, `UsageRecord`

### FR: Subject Attribution

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-fr-subject-attribution`

The system **MUST** treat `SubjectRef` as caller-supplied and optional — `subject_id` is the only mandatory component when subject attribution is supplied, `subject_type` is optional and MUST NOT be supplied without `subject_id` per the optional-subject rule — materialize the supplied components on every persisted record via the Plugin SPI persist capability, omit the entity entirely for system-level consumption, include subject attribution in the per-record attribution tuple sent to `cpt-cf-usage-collector-flow-foundation-pdp-authorize` when present, and never synthesize subject attribution server-side.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`
- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-adr-caller-supplied-attribution`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `SubjectRef`, `UsageRecord`

### FR: Ingestion Authorization

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-fr-ingestion-authorization`

The system **MUST** accept an inbound `SecurityContext` at both ingestion entry points — on REST as `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`), on the SDK trait as `ctx: &SecurityContext` first parameter to `UsageCollectorClientV1::create_usage_record` / `create_usage_records` per `sdk-trait.md` Methods 1 and 2 — and authorize every ingestion call by dispatching per-attribution-tuple PDP authorization to `cpt-cf-usage-collector-flow-foundation-pdp-authorize` through the per-component `authz_scope` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) inside `cpt-cf-usage-collector-component-ingestion-gateway` against the full attribution tuple (`tenant_id`, `ResourceRef`, optional `SubjectRef`, UsageType `gts_id`); fail closed when the inbound `SecurityContext` is missing (canonical `Unauthenticated` `Problem` envelope per the `usage-collector-v1.yaml` `default` response) or when the PDP resolver is unavailable (no synthesized identity, no cached PDP decision); surface per-tuple PDP `deny` decisions as per-record `outcome="rejected"` with `context.reason="AUTHZ"` inside `CreateUsageRecordsResponse` — never as whole-request rejection, because PDP authorization is per attribution tuple and there is no envelope-level PDP deny aggregation — without any Plugin SPI dispatch in either case.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`

**Constraints**: `cpt-cf-usage-collector-principle-fail-closed`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-ingestion-gateway`
- Entities: `SecurityContext`

### FR: Usage Compensation — Flow

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-compensation-flow`

The system **MUST** accept counter value-reversal records on the **same unified ingestion path** as ordinary usage emission (`POST /usage-collector/v1/records` and the SDK emit operation routed through `cpt-cf-usage-collector-component-ingestion-gateway`) without introducing a dedicated `compensate` REST path, SDK method, or Plugin SPI call and DESIGN §3.3 "Unified ingestion request shape"; persist accepted compensation records as a `UsageRecord` with a strictly-negative signed `value` and a non-empty `corrects_id` pointing at the referenced ordinary usage row (one with `corrects_id IS NULL`), under the same PDP attribution (`cpt-cf-usage-collector-flow-foundation-pdp-authorize` via the per-component `authz_scope` helper) and the same mandatory caller-supplied `IdempotencyKey` (`cpt-cf-usage-collector-adr-mandatory-idempotency`) that govern ordinary ingestion; surface the per-record acknowledgement (`outcome="accepted"` for both a fresh compensation insert and a silent-absorb exact-equality retry, or `outcome="rejected"` for any per-record failure including `IDEMPOTENCY_CONFLICT`) in the same `CreateUsageRecordsResponse` shape. Recording a caller-supplied signed-negative `value` is recording, not computing — the system MUST NOT compute refunds, credits, credit-notes, or quota; mandatory idempotency prevents double-refund for free.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-compensation`
- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`
- `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`

**Constraints**: `cpt-cf-usage-collector-fr-usage-compensation`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `UsageRecord`

### FR: Usage Compensation — Value Matrix

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-value-matrix`

The system **MUST** enforce the locked four-cell `(MetricSemantics × corrects_id presence)` value matrix at validation time via `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` before any Plugin SPI persist dispatch: `counter` + `corrects_id IS NULL` requires `value >= 0` (unchanged); `counter` + `corrects_id SET` requires `value < 0` (strictly negative — zero is not accepted); `gauge` + `corrects_id IS NULL` accepts any signed value (unchanged); `gauge` + `corrects_id SET` is **REJECTED** before persistence (gauges have no `SUM` semantics; the only correction for a gauge is deactivation). Value-sign violations of the `counter` + `corrects_id IS NULL` (`value < 0`) and `counter` + `corrects_id SET` (`value >= 0`) cells surface as the per-record `outcome="rejected"` with `context.reason="SEMANTICS_VIOLATION"` per `usage-collector-v1.yaml`; the `gauge` + `corrects_id SET` cell surfaces with `context.reason="GAUGE_COMPENSATION_REJECTED"` (HTTP `422`) per the locked five-code compensation taxonomy in `usage-collector-v1.yaml` and the SDK trait `GaugeCompensationRejected` variant — it is NEVER collapsed into the generic `SEMANTICS_VIOLATION` code.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`
- `cpt-cf-usage-collector-flow-usage-emission-compensation`

**Constraints**: `cpt-cf-usage-collector-fr-usage-compensation`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `UsageType`, `UsageRecord`

### FR: Usage Compensation — L1 corrects_id

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-corrects-id-l1`

The system **MUST** enforce the L1 `corrects_id` referential rule synchronously at ingestion via `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` whenever the submitted `UsageRecord` carries `corrects_id` set: (1) the referenced row MUST exist; (2) the referenced row MUST itself be an ordinary usage row (`corrects_id IS NULL`) — compensating a compensation is a non-goal; (3) the referenced row MUST share the full identity tuple `(tenant_id, gts_id, resource_ref, subject_ref)` with the incoming compensation (cross-tenant, cross-usage-type, cross-resource, cross-subject, or `subject_ref` presence mismatches are rejected — `None` vs `Some(_)` is a scope error); (4) the referenced row MUST be `status = active`. Failures (1)-(4) surface as `outcome="rejected"` with one of the four granular wire codes from the locked `usage-collector-v1.yaml` Problem.context.reason taxonomy: rule (1) → `CORRECTS_ID_NOT_FOUND` (HTTP `404`); rule (2) → `CORRECTS_ID_TARGETS_COMPENSATION` (HTTP `409`); rule (3) → `CORRECTS_ID_WRONG_SCOPE` (HTTP `409`); rule (4) → `CORRECTS_ID_INACTIVE` (HTTP `409`) — these codes are NEVER collapsed into a single generic `corrects_id_invalid` code (no such code is declared in the OpenAPI taxonomy or in `sdk-trait.md`). The mapping from algorithm outcomes to these wire codes is the contract recorded in the algorithm's "Algorithm outcome → wire `context.reason` mapping" table. The system MUST NOT track L2 per-record remaining amounts, lots, FIFO/LIFO ordering, or non-negative net.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`
- `cpt-cf-usage-collector-flow-usage-emission-compensation`

**Constraints**: `cpt-cf-usage-collector-fr-usage-compensation`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `UsageRecord`

### FR: Usage Compensation — Concurrency

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-compensation-concurrency`

The system **MUST** reject a compensation whose referenced ordinary usage row (`corrects_id IS NULL`) is mid-deactivation via the L1 "referenced record MUST be `active`" check inside `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` — there is no quarantine, no retry queue, no compensating cascade for the rejection, and no additional distributed-coordination machinery is added at the gateway. A compensation referencing a row R that arrives while R is being deactivated is rejected by the same L1 "active" check that handles fully-inactive references; the caller retries at its own discretion and the mandatory idempotency key makes those retries safe (the depth-1 cascade itself is owned by `cpt-cf-usage-collector-feature-event-deactivation` and `cpt-cf-usage-collector-adr-monotonic-deactivation`, not by this feature).

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`
- `cpt-cf-usage-collector-flow-usage-emission-compensation`

**Constraints**: `cpt-cf-usage-collector-fr-usage-compensation`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `UsageRecord`, `UsageRecordStatus`

### FR: Usage Compensation — No Business Logic

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-compensation-no-business-logic`

The system **MUST NOT** compute refunds, credits, credit-notes, quota, lot/FIFO-LIFO state, or per-record remaining amounts when recording a compensation. Recording a caller-supplied signed-negative `value` on a `UsageRecord` whose `corrects_id` is set is **recording, not computing** — the caller owns the business decision to give back capacity (capacity refund, partial cancellation, dispute resolution, billing-period correction); the Usage Collector validates the four-cell matrix + the L1 `corrects_id` rule and persists the row as-supplied. The system MUST NOT validate non-negative net at write time and MUST NOT emit a negative-net detection signal per the un-policed-net stance recorded in `cpt-cf-usage-collector-adr-usage-compensation`; downstream consumers (billing, quota, FinOps) own any "net can't be negative" policy. Mandatory idempotency prevents double-refund for free.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-compensation`
- `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`

**Constraints**: `cpt-cf-usage-collector-constraint-no-business-logic`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `UsageRecord`

### NFR: Throughput

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-nfr-throughput`

The system **MUST** sustain the target steady-state ingestion throughput floor declared by `cpt-cf-usage-collector-nfr-throughput` end-to-end through `cpt-cf-usage-collector-component-ingestion-gateway` and the Plugin SPI persist capability, with no degradation under continuous load and no throughput regression introduced by per-record validation (attribution + PDP authorization, catalog lookup, semantics enforcement, metadata size-cap enforcement) or per-record SPI dispatch on the synchronous write path.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-constraint-nfr-thresholds`

**Touches**:

- API: `POST /usage-collector/v1/records`

### NFR: Throughput Profile

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-nfr-throughput-profile`

The system **MUST** preserve the `cpt-cf-usage-collector-nfr-throughput` floor under the mixed counter/gauge workload profile declared by `cpt-cf-usage-collector-nfr-throughput-profile` so that neither kind starves the other under contention — `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` MUST distinguish the counter and gauge branches deterministically without introducing kind-dependent backpressure asymmetry at the Ingestion Gateway, and the Plugin SPI dispatch path MUST treat both kinds uniformly through the single composite key contract.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`
- `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`

**Constraints**: `cpt-cf-usage-collector-constraint-nfr-thresholds`

**Touches**:

- API: `POST /usage-collector/v1/records`

### NFR: Ingestion Latency

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-nfr-ingestion-latency`

The system **MUST** hold the per-record ingestion-latency budget declared by `cpt-cf-usage-collector-nfr-ingestion-latency` end-to-end through `cpt-cf-usage-collector-component-ingestion-gateway`, the per-component `authz_scope` helper invocation against `cpt-cf-usage-collector-contract-authz-resolver`, the per-record `get_usage_type` SPI dispatch for UsageType existence-and-semantics lookup, and the Plugin SPI persist capability — and report it through the `usage_collector.ingestion.latency` histogram (declared with `with_unit("s")`) whose bucket layout brackets the published 200 ms p95 budget per DESIGN §3.11.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup`

**Constraints**: `cpt-cf-usage-collector-constraint-nfr-thresholds`

**Touches**:

- API: `POST /usage-collector/v1/records`

### NFR: Workload Isolation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-nfr-workload-isolation`

The system **MUST** isolate the synchronous ingestion path from the read-side query path and from operator-side UsageType and deactivation paths so that sustained read or lifecycle workload does not degrade ingestion latency or throughput beyond the `cpt-cf-usage-collector-nfr-ingestion-latency` and `cpt-cf-usage-collector-nfr-throughput` budgets — `cpt-cf-usage-collector-component-ingestion-gateway` MUST be the sole entry point for the write path, with no shared mutable state or shared backpressure with the read-side or operator-side gateways beyond the single Plugin Host dispatch surface.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-nfr-workload-isolation`

**Touches**:

- Component: `cpt-cf-usage-collector-component-ingestion-gateway`

### Principle: Idempotency by Key

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-principle-idempotency-by-key`

The system **MUST** uphold idempotency-by-key as the at-least-once delivery contract: every record carries a caller-supplied `IdempotencyKey`, the dedup boundary is the Plugin SPI composite key `(tenant_id, gts_id, idempotency_key)`, and EXACT-EQUALITY retries sharing the composite (ALL caller canonical fields equal) silently absorb at the SPI — the plugin returns `Ok(UsageRecord)` carrying the previously persisted body, and the wire surface returns `outcome="accepted"` indistinguishably from a fresh insert without a second write, so counter totals MUST NOT be inflated by retries — uniformly across counter and gauge kinds. The silent absorb is reserved EXCLUSIVELY for exact-equality retries: a same-key submission whose canonical fields differ MUST instead surface from the SPI as `Err(UsageCollectorPluginError::IdempotencyConflict { idempotency_key, gts_id })` lifted to `outcome="rejected"` with `context.reason="IDEMPOTENCY_CONFLICT"` (AlreadyExists/409) and MUST NOT be silently dropped.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-principle-idempotency-by-key`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `IdempotencyKey`

### Principle: Semantics Enforcement

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-principle-semantics-enforcement`

The system **MUST** enforce `UsageType` invariants at `cpt-cf-usage-collector-component-ingestion-gateway` before any usage-record Plugin SPI write dispatch — counter records MUST satisfy non-negative `value` per `cpt-cf-usage-collector-adr-usage-compensation`, gauge records are accepted as-is per DESIGN §3.1 — by confirming UsageType existence via `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` (a per-record `get_usage_type` SPI dispatch against `cpt-cf-usage-collector-contract-storage-plugin`) and reading `UsageType.kind` from the catalog row via `UsageType::is_counter` / `UsageType::is_gauge`.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`
- `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup`

**Constraints**: `cpt-cf-usage-collector-principle-semantics-enforcement`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-usage-type-catalog`
- Entities: `UsageType`, `UsageRecord`

### Principle: Fail Closed

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-principle-fail-closed`

The system **MUST** fail closed on every boundary or downstream-resolver or Plugin SPI unavailability: when the inbound `SecurityContext` is missing at the handler boundary (REST handler did not receive `Extension<SecurityContext>` from ToolKit gateway middleware, or the SDK trait was invoked without a `ctx` argument) the ingestion path MUST return the canonical `Unauthenticated` `Problem` envelope without any PDP call or record dispatch; when the `cpt-cf-usage-collector-contract-authz-resolver` PDP resolver is unreachable or denies, `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (invoked via the per-component `authz_scope` helper inside `cpt-cf-usage-collector-component-ingestion-gateway`) propagates the canonical `PermissionDenied` `Problem` envelope without record dispatch; when the Plugin SPI surfaces transport / readiness / persistence errors the per-record outcome is `rejected` (`context.reason="PLUGIN_READINESS"`) with no acknowledged record — no synthesized identity, no cached PDP decision, and no invented storage binding per the `cpt-cf-usage-collector-component-ingestion-gateway` boundary.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`

**Constraints**: `cpt-cf-usage-collector-principle-fail-closed`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-ingestion-gateway`

### Principle: Pluggable Storage

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-principle-pluggable-storage`

The system **MUST** dispatch every persistence call through the foundation-owned Plugin Host and the Plugin SPI single-record (Method 1 `create_usage_record`) or batch (Method 2 `create_usage_records`) capability — never embedding backend-specific SQL, schema, or client code in `cpt-cf-usage-collector-component-ingestion-gateway`, never opening a parallel storage path, and never inventing a binding when the registry or orchestrator is unreachable — so that the active storage plugin can be swapped via operator configuration without touching the ingestion path.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-principle-pluggable-storage`

**Touches**:

- API: `POST /usage-collector/v1/records`

### Constraint: No Business Logic

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-constraint-no-business-logic`

The system **MUST** keep the ingestion path free of billing, pricing, quota enforcement, and per-UsageType payload-content interpretation — `cpt-cf-usage-collector-component-ingestion-gateway` MUST NOT interpret `RecordMetadata` content, MUST NOT apply any per-tenant or per-UsageType accounting transform, and MUST NOT mutate the `value` field beyond semantics-violation rejection — every business rule is owned by callers and downstream consumers.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`
- `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement`

**Constraints**: `cpt-cf-usage-collector-constraint-no-business-logic`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `RecordMetadata`, `UsageRecord`

### Constraint: NFR Thresholds

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-constraint-nfr-thresholds`

The system **MUST** hold the PRD-declared NFR thresholds end-to-end on the ingestion path — `cpt-cf-usage-collector-nfr-throughput`, `cpt-cf-usage-collector-nfr-throughput-profile`, `cpt-cf-usage-collector-nfr-ingestion-latency`, `cpt-cf-usage-collector-nfr-workload-isolation` — surfacing them through the `usage_collector.ingestion.requests` counter and the `usage_collector.ingestion.latency` histogram (declared with `with_unit("s")`) as the operator-side instruments per DESIGN §3.11 so each threshold is independently observable.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-constraint-nfr-thresholds`

**Touches**:

- API: `POST /usage-collector/v1/records`

### ADR: Caller-supplied Attribution

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-adr-caller-supplied-attribution`

The system **MUST** consume `tenant_id`, `ResourceRef`, and optional `SubjectRef` exclusively from the caller — carried verbatim on the per-record `UsageRecord` payload (the caller-supplied `tenant_id` field is the authoritative tenant scope for the record and MUST NOT be derived from the inbound `SecurityContext`) — and MUST NOT synthesize any of these fields server-side, MUST NOT derive them from headers other than the caller-bound credential material, and MUST NOT permit operator overrides on the ingestion path.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`
- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-adr-caller-supplied-attribution`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `SecurityContext`, `ResourceRef`, `SubjectRef`

### ADR: Mandatory Idempotency

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-adr-mandatory-idempotency`

The system **MUST** require a caller-supplied `IdempotencyKey` on every `UsageRecord` payload at the wire level — `idempotency_key` is a mandatory attribution field on every persisted record per the SPI dedup composite (`plugin-spi.md` §"Cross-entity invariants honored by the Plugin SPI") — reject submissions missing the key with the actionable validation error envelope before any Plugin SPI dispatch, dedup retries under the Plugin SPI composite `(tenant_id, gts_id, idempotency_key)`, and surface duplicate outcomes uniformly across counter and gauge kinds so retries never inflate counter totals or poison gauge point-in-time signals.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-adr-mandatory-idempotency`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `IdempotencyKey`

### Component: Ingestion Gateway

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-component-ingestion-gateway`

The system **MUST** realize `cpt-cf-usage-collector-component-ingestion-gateway` as the sole synchronous write entry point for usage records (REST and SDK, single and batched), owning the ingestion contract end-to-end — SecurityContext acceptance at both entry points (REST handler with `Extension<SecurityContext>` from ToolKit gateway middleware via `OperationBuilder::authenticated()`; SDK trait `create_usage_record(ctx, ...)` / `create_usage_records(ctx, ...)` with `ctx: &SecurityContext` as the first parameter), per-component PDP enforcement via the `authz_scope` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`), structural attribution-tuple validation, mandatory idempotency-key requirement, semantics-dependent invariants resolved per record via a `get_usage_type` SPI dispatch against `cpt-cf-usage-collector-contract-storage-plugin`, fixed 8 KiB `RecordMetadata` size-cap enforcement, deterministic per-record acknowledgements — while delegating persistence to `cpt-cf-usage-collector-component-plugin-host` and the UsageType catalog SoR to `cpt-cf-usage-collector-component-usage-type-catalog`, with no PDP-decision caching, no synthesized identities, no invented storage bindings, and no interpretation of `RecordMetadata` content per DESIGN §3.2.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`
- `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`
- `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`
- `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement`
- `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup`

**Constraints**: `cpt-cf-usage-collector-component-ingestion-gateway`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `UsageRecord`

### Sequence: Emit Usage Record

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-seq-emit-usage`

The system **MUST** implement the `cpt-cf-usage-collector-seq-emit-usage` sequence end-to-end: caller surface (REST handler receiving `Extension<SecurityContext>` from ToolKit gateway middleware, or SDK trait `create_usage_record(ctx, ...)` / `create_usage_records(ctx, ...)` with `ctx: &SecurityContext` first) → Ingestion Gateway per-component PDP authorization via the `authz_scope` helper against `cpt-cf-usage-collector-contract-authz-resolver` → Ingestion Gateway dispatch → UsageType Catalog existence/semantics lookup → Plugin Host → storage plugin persist under the composite key `(tenant_id, gts_id, idempotency_key)` → per-record acknowledgement, with PDP denial, unknown UsageType, semantics-violation violation, oversize metadata, and SPI errors rejecting or marking per-record outcomes without any whole-batch rollback per DESIGN §3.6.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-seq-emit-usage`

**Touches**:

- API: `POST /usage-collector/v1/records`


### Entity: Usage Record

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-entity-usage-record`

The system **MUST** treat `UsageRecord` per DESIGN §3.1 as a single attributed measurement of resource consumption carrying `value`, attribution tuple (`tenant_id`, `ResourceRef`, optional `SubjectRef`, UsageType `gts_id`), caller-supplied `IdempotencyKey`, `status`, service-set `created_at`, and optional `RecordMetadata`; the entity MUST be append-only after acceptance except for the `status` transition owned by the deactivation feature, and every field carried on the entity at acceptance time MUST be materialized verbatim on the persisted record through the Plugin SPI persist capability.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `UsageRecord`

**Touches**:

- Entities: `UsageRecord`

### Entity: Record Metadata

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-entity-record-metadata`

The system **MUST** treat `RecordMetadata` per DESIGN §3.1 as an optional opaque JSON object carried verbatim on a `UsageRecord` — never indexed, never aggregated, never interpreted by `cpt-cf-usage-collector-component-ingestion-gateway` or the Plugin Host — persisted byte-for-byte through the Plugin SPI per the `sdk-trait.md` Method 1 invariant and `plugin-spi.md` Method 1 invariant 2, and bounded by the fixed 8 KiB cap enforced before any SPI dispatch.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement`

**Constraints**: `RecordMetadata`

**Touches**:

- Entities: `RecordMetadata`

### Entity: Resource Ref

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-entity-resource-ref`

The system **MUST** treat `ResourceRef` per DESIGN §3.1 as a caller-supplied composite identifying the attributed resource — mandatory `resource_id` plus mandatory `resource_type` — required on every `UsageRecord` payload, materialized on every persisted record as mandatory `resource_id` and `resource_type` attribution via the Plugin SPI persist capability, and forwarded as part of the per-record attribution tuple to `cpt-cf-usage-collector-flow-foundation-pdp-authorize`.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`
- `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`

**Constraints**: `ResourceRef`

**Touches**:

- Entities: `ResourceRef`

### Entity: Subject Ref

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-entity-subject-ref`

The system **MUST** treat `SubjectRef` per DESIGN §3.1 as a caller-supplied subject attribution — mandatory `subject_id` plus optional `subject_type` when supplied; omitted entirely for system-level consumption — and per the optional-subject rule MUST NOT accept `subject_type` without `subject_id`, materializing the supplied components verbatim on the persisted record as optional `subject_id` and `subject_type` attribution via the Plugin SPI persist capability, and forwarding subject attribution as part of the per-record attribution tuple to `cpt-cf-usage-collector-flow-foundation-pdp-authorize` when present.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`
- `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`

**Constraints**: `SubjectRef`

**Touches**:

- Entities: `SubjectRef`

### Entity: Idempotency Key

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-entity-idempotency-key`

The system **MUST** treat `IdempotencyKey` per DESIGN §3.1 as a caller-supplied opaque identifier that deduplicates retried submissions uniformly across counter and gauge kinds, require its presence on every `UsageRecord` payload, materialize it on every persisted record as mandatory `idempotency_key` attribution via the Plugin SPI persist capability, and participate in the Plugin SPI composite UNIQUE key `(tenant_id, gts_id, idempotency_key)` that drives dedup.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `IdempotencyKey`

**Touches**:

- Entities: `IdempotencyKey`

### Entity: UsageType

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-entity-usage-type`

The system **MUST** consume `UsageType` per DESIGN §3.1 as a read-only catalog reference on the ingestion path — the ingestion path NEVER mutates UsageTypes — resolving UsageType existence and the declared `metadata_fields` for a target `gts_id` exclusively through `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` (a per-record `get_usage_type` SPI dispatch against `cpt-cf-usage-collector-contract-storage-plugin`), with absent entries surfaced as `not-found`. Counter / gauge accumulation semantics are read at the call site from the catalog row's closed `UsageKind` enum (`UsageType.kind == Counter` ⇒ counter; `UsageType.kind == Gauge` ⇒ gauge) and drive the counter and gauge branches of `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` deterministically.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup`
- `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`

**Constraints**: `UsageType`

**Touches**:

- Component: `cpt-cf-usage-collector-component-usage-type-catalog`
- Entities: `UsageType`

### Entity: Security Context

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-entity-security-context`

The system **MUST** consume `SecurityContext` (see `domain-model.md` §2.7) as the platform-resolved caller-identity envelope (caller principal, caller's tenant scope, auxiliary claims) — never owned, synthesized, or cached by `cpt-cf-usage-collector-component-ingestion-gateway`. The handler MUST accept the `SecurityContext` exclusively at one of the two convention-bound entry points — on REST as `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`) and on the SDK trait as `ctx: &SecurityContext` passed as the first parameter to `UsageCollectorClientV1::create_usage_record(ctx, ...)` / `create_usage_records(ctx, ...)` per `sdk-trait.md` Methods 1 and 2 — and pass it verbatim to `cpt-cf-usage-collector-flow-foundation-pdp-authorize` via the per-component `authz_scope` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) so PDP authorizes the caller's identity against each per-record attribution tuple (including the caller-supplied `tenant_id` from `UsageRecordInput`), and fail closed on missing `SecurityContext` or PDP unavailability. The `SecurityContext` is the subject of PDP authorization — the persisted record's `tenant_id` attribution is materialized from the caller-supplied `UsageRecordInput.tenant_id` field, not synthesized from the SecurityContext's tenant scope.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-usage-emission-attribution-and-pdp-authorization`

**Constraints**: `SecurityContext`

**Touches**:

- Component: `cpt-cf-usage-collector-component-ingestion-gateway`
- Entities: `SecurityContext`

### API: POST /usage-collector/v1/records

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-emission-api-post-records`

The system **MUST** expose `POST /usage-collector/v1/records` as the sole REST write entry point per `usage-collector-v1.yaml`, with the REST handler receiving `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`) and delegating to `UsageCollectorClientV1::create_usage_record` / `create_usage_records` (`ctx: &SecurityContext` as first parameter per `sdk-trait.md` Methods 1 and 2), accepting an `CreateUsageRecordsRequest` with `records` `minItems: 1` / `maxItems: 100`, returning the `CreateUsageRecordsResponse` with per-record outcomes in input order under HTTP `200` (all accepted) or HTTP `207 Multi-Status` (mixed or all-rejected — i.e., whenever ≥1 record is rejected, single-record conflict included), and surfacing deterministic `Problem` envelopes only for whole-request failures (missing `SecurityContext` surfaced as canonical `Unauthenticated`; structural request-body validation) per the yaml's `default` response — per-record errors (PDP `deny`, unknown UsageType, semantics violation, metadata size, plugin SPI error) MUST surface as the per-record `outcome="rejected"` with a per-record `error: Problem` carrying the `context.reason` drawn from the cause-category taxonomy declared in the yaml, never widening the contract beyond what is declared in the yaml.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-emission-emit-record`
- `cpt-cf-usage-collector-flow-usage-emission-emit-records-batch`

**Constraints**: `cpt-cf-usage-collector-fr-ingestion`

**Touches**:

- API: `POST /usage-collector/v1/records`
- Entities: `UsageRecord`

### §2.3-item → DoD-ID Coverage Matrix

Coverage of every DECOMPOSITION §2.3 catalog item:

| §2.3 Item                                                                                                  | Kind              | DoD ID                                                                                                                                  |
| ---------------------------------------------------------------------------------------------------------- | ----------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `cpt-cf-usage-collector-fr-ingestion`                                                                      | FR                | `cpt-cf-usage-collector-dod-usage-emission-fr-ingestion`                                                                                |
| `cpt-cf-usage-collector-fr-idempotency`                                                                    | FR                | `cpt-cf-usage-collector-dod-usage-emission-fr-idempotency`                                                                              |
| `cpt-cf-usage-collector-fr-record-metadata`                                                                | FR                | `cpt-cf-usage-collector-dod-usage-emission-fr-record-metadata`                                                                          |
| `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`                                             | FR                | `cpt-cf-usage-collector-dod-usage-emission-fr-usage-type-existence-and-semantics`                                                       |
| `cpt-cf-usage-collector-fr-counter-semantics`                                                              | FR                | `cpt-cf-usage-collector-dod-usage-emission-fr-counter-semantics`                                                                        |
| `cpt-cf-usage-collector-fr-gauge-semantics`                                                                | FR                | `cpt-cf-usage-collector-dod-usage-emission-fr-gauge-semantics`                                                                          |
| `cpt-cf-usage-collector-fr-tenant-attribution`                                                             | FR                | `cpt-cf-usage-collector-dod-usage-emission-fr-tenant-attribution`                                                                       |
| `cpt-cf-usage-collector-fr-resource-attribution`                                                           | FR                | `cpt-cf-usage-collector-dod-usage-emission-fr-resource-attribution`                                                                     |
| `cpt-cf-usage-collector-fr-subject-attribution`                                                            | FR                | `cpt-cf-usage-collector-dod-usage-emission-fr-subject-attribution`                                                                      |
| `cpt-cf-usage-collector-fr-ingestion-authorization`                                                        | FR                | `cpt-cf-usage-collector-dod-usage-emission-fr-ingestion-authorization`                                                                  |
| `cpt-cf-usage-collector-nfr-throughput`                                                                    | NFR               | `cpt-cf-usage-collector-dod-usage-emission-nfr-throughput`                                                                              |
| `cpt-cf-usage-collector-nfr-throughput-profile`                                                            | NFR               | `cpt-cf-usage-collector-dod-usage-emission-nfr-throughput-profile`                                                                      |
| `cpt-cf-usage-collector-nfr-ingestion-latency`                                                             | NFR               | `cpt-cf-usage-collector-dod-usage-emission-nfr-ingestion-latency`                                                                       |
| `cpt-cf-usage-collector-nfr-workload-isolation`                                                            | NFR               | `cpt-cf-usage-collector-dod-usage-emission-nfr-workload-isolation`                                                                      |
| `cpt-cf-usage-collector-principle-idempotency-by-key`                                                      | Principle         | `cpt-cf-usage-collector-dod-usage-emission-principle-idempotency-by-key`                                                                |
| `cpt-cf-usage-collector-principle-semantics-enforcement`                                                        | Principle         | `cpt-cf-usage-collector-dod-usage-emission-principle-semantics-enforcement`                                                                  |
| `cpt-cf-usage-collector-principle-fail-closed`                                                             | Principle         | `cpt-cf-usage-collector-dod-usage-emission-principle-fail-closed`                                                                       |
| `cpt-cf-usage-collector-principle-pluggable-storage`                                                       | Principle         | `cpt-cf-usage-collector-dod-usage-emission-principle-pluggable-storage`                                                                 |
| `cpt-cf-usage-collector-constraint-no-business-logic`                                                      | Design constraint | `cpt-cf-usage-collector-dod-usage-emission-constraint-no-business-logic`                                                                |
| `cpt-cf-usage-collector-constraint-nfr-thresholds`                                                         | Design constraint | `cpt-cf-usage-collector-dod-usage-emission-constraint-nfr-thresholds`                                                                   |
| `cpt-cf-usage-collector-adr-caller-supplied-attribution`                                                   | ADR               | `cpt-cf-usage-collector-dod-usage-emission-adr-caller-supplied-attribution`                                                             |
| `cpt-cf-usage-collector-adr-mandatory-idempotency`                                                         | ADR               | `cpt-cf-usage-collector-dod-usage-emission-adr-mandatory-idempotency`                                                                   |
| `cpt-cf-usage-collector-component-ingestion-gateway`                                                       | Design component  | `cpt-cf-usage-collector-dod-usage-emission-component-ingestion-gateway`                                                                 |
| `cpt-cf-usage-collector-seq-emit-usage`                                                                    | Sequence          | `cpt-cf-usage-collector-dod-usage-emission-seq-emit-usage`                                                                              |
| `UsageRecord`                                                               | Entity            | `cpt-cf-usage-collector-dod-usage-emission-entity-usage-record`                                                                         |
| `RecordMetadata`                                                            | Entity            | `cpt-cf-usage-collector-dod-usage-emission-entity-record-metadata`                                                                      |
| `TenantRef` (carried via `SecurityContext`; materialized as the `tenant_id` column per DECOMPOSITION §2.3) | Entity            | `cpt-cf-usage-collector-dod-usage-emission-entity-security-context` / `cpt-cf-usage-collector-dod-usage-emission-fr-tenant-attribution` |
| `ResourceRef`                                                               | Entity            | `cpt-cf-usage-collector-dod-usage-emission-entity-resource-ref`                                                                         |
| `SubjectRef`                                                                | Entity            | `cpt-cf-usage-collector-dod-usage-emission-entity-subject-ref`                                                                          |
| `IdempotencyKey`                                                            | Entity            | `cpt-cf-usage-collector-dod-usage-emission-entity-idempotency-key`                                                                      |
| `UsageType`                                                                 | Entity            | `cpt-cf-usage-collector-dod-usage-emission-entity-usage-type`                                                                           |
| `UsageType`                                                                 | Entity            | `cpt-cf-usage-collector-dod-usage-emission-entity-usage-type`                                                                           |
| `SecurityContext`                                                           | Entity            | `cpt-cf-usage-collector-dod-usage-emission-entity-security-context`                                                                     |
| `POST /usage-collector/v1/records`                                                                         | API               | `cpt-cf-usage-collector-dod-usage-emission-api-post-records`                                                                            |

## 6. Acceptance Criteria

- [ ] `p1` - A well-formed single-record emit by an authorized caller through `POST /usage-collector/v1/records` (one-item `CreateUsageRecordsRequest`) or the SDK single-emit operation persists exactly one durable record through the Plugin SPI Method 1 single-record persist capability; the persisted record's `tenant_id`, `resource_id`, `resource_type`, optional `subject_id` / `subject_type`, `gts_id`, `value`, `idempotency_key`, and `metadata` attribution is byte-identical to the request payload (with service-set `created_at` and `status="active"` materialized on insert), and the per-record acknowledgement carries `outcome="accepted"` with the caller-supplied `uuid` per `usage-collector-v1.yaml` (single-emit success).
- [ ] `p1` - A batch `POST /usage-collector/v1/records` carrying N `UsageRecord` payloads with 1 ≤ N ≤ 100 (`CreateUsageRecordsRequest.records` `minItems: 1` / `maxItems: 100`) dispatches each eligible-for-persist record through the Plugin SPI Method 2 batch persist capability under the per-record composite key `(tenant_id, gts_id, idempotency_key)` and returns per-record outcomes in input order; a request with N > 100 or with an empty `records` array is rejected at the Ingestion Gateway with the request-level structural validation `Problem` envelope (HTTP `400`) per `usage-collector-v1.yaml` before any per-record processing and no row is written (batch cap and per-record dispatch).
- [ ] `p1` - A counter-kind emit (resolved through `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup`) whose submitted `value` is below zero is rejected with the actionable validation error envelope (per-record `outcome="rejected"` with `context.reason="SEMANTICS_VIOLATION"`) before any Plugin SPI dispatch; counter records persisted by accepted emits satisfy the counter non-negativity invariant `value >= 0`.
- [ ] `p1` - A gauge-kind emit is accepted as a point-in-time value stored as-is — no delta accumulation, no rewriting, no server-side shape rewriting — per DESIGN §3.1; a subsequent gauge emit for the same `(tenant_id, gts_id)` pair replaces the prior value as observed by downstream readers (gauge replacement semantics).
- [ ] `p1` - Two emits sharing the same composite `(tenant_id, gts_id, idempotency_key)` within the Plugin SPI's dedup window — including across counter and gauge kinds — result in exactly one persisted record through the Plugin SPI; the second call returns `outcome="accepted"` carrying the previously-persisted body (the SPI returns `Ok(UsageRecord)` indistinguishably from a fresh insert — silent absorb), without performing a second write, counter totals are not inflated, and the dedup check and the persist are a single SPI capability invocation (Method 1 for single, Method 2 for batch) with no separate non-transactional pre-check (idempotency dedup).
- [ ] `p1` - An emit whose canonical on-the-wire serialized `RecordMetadata` exceeds the configured size cap (default 8 KiB per record) is rejected with the per-record `outcome="rejected"` carrying a per-record `error: Problem` (`context.reason="METADATA_SIZE"`) with the measured size and the configured cap before any Plugin SPI dispatch; payloads at or below the cap are forwarded unmodified and persisted byte-for-byte through the Plugin SPI with no truncation, rewriting, or content interpretation (metadata cap).
- [ ] `p1` - Every accepted single or batched emit accepts a resolved `SecurityContext` at the handler boundary — on REST as `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`), on the SDK trait as `ctx: &SecurityContext` first parameter — and dispatches per-attribution-tuple PDP authorization through `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (per-component `authz_scope` helper against `cpt-cf-usage-collector-contract-authz-resolver`) against the full attribution tuple (`tenant_id` from `UsageRecordInput`, `ResourceRef`, optional `SubjectRef`, UsageType `gts_id`) before any Plugin SPI dispatch; absence of `SecurityContext` at the boundary surfaces the canonical `Unauthenticated` `Problem` envelope per the yaml `default` response, a per-tuple PDP `deny` surfaces the per-record `outcome="rejected"` with `context.reason="AUTHZ"` inside `CreateUsageRecordsResponse` (PDP decisions are per-tuple — there is no envelope-level PDP deny aggregation), and no row is written in any of these cases (PDP-gated attribution authorization).
- [ ] `p1` - Every emit resolves UsageType existence and `UsageType` exclusively through `cpt-cf-usage-collector-algo-usage-emission-catalog-existence-and-kind-lookup` (a per-record `get_usage_type` SPI dispatch against `cpt-cf-usage-collector-contract-storage-plugin`) before any usage-record Plugin SPI write dispatch; an emit whose UsageType `gts_id` is absent from the plugin's `usage_type_catalog` — or whose `gts_id`-prefix-derived semantics disagrees with the per-record candidate once semantics enforcement runs — surfaces the per-record `outcome="rejected"` with `context.reason="UNKNOWN_USAGE_TYPE"` for absence or `context.reason="SEMANTICS_VIOLATION"` for kind mismatch (catalog existence and semantics enforcement, `cpt-cf-usage-collector-dod-usage-emission-principle-semantics-enforcement`, and `cpt-cf-usage-collector-dod-usage-emission-entity-usage-type`).
- [ ] `p1` - Every accepted emit — single or batched — is persisted through the foundation-owned Plugin Host via the Plugin SPI Method 1 `create_usage_record` or Method 2 `create_usage_records` capability under the composite key `(tenant_id, gts_id, idempotency_key)` and the SPI durability ack is required before the per-record `outcome="accepted"` is returned to the caller; Plugin SPI transport / readiness / persistence errors (`PluginUnavailable`, `Timeout`, `BackendError`) surface as per-record `rejected` outcomes (`context.reason="PLUGIN_READINESS"`), no whole-batch rollback occurs, and the ingestion path never opens a parallel storage path or invents a binding (at-least-once durability through the Plugin SPI and `cpt-cf-usage-collector-dod-usage-emission-seq-emit-usage`).
- [ ] `p1` - A well-formed compensation emit by an authorized caller — `value < 0`, a non-empty `corrects_id` pointing to an ordinary usage row that exists, has `corrects_id IS NULL`, shares `(tenant_id, gts_id)`, and is `status=active`, plus a mandatory caller-supplied idempotency key — is accepted on the **same unified ingestion path** (`POST /usage-collector/v1/records` or the SDK emit operation; no dedicated `compensate` endpoint, SDK method, or Plugin SPI call exists) and persists exactly one `UsageRecord` with the signed-negative `value` and the `corrects_id` pointer; the per-record acknowledgement carries `outcome="accepted"` with the caller-supplied `uuid` (compensation flow success).
- [ ] `p1` - A compensation emit against a `gauge` UsageType is rejected at validation time via `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` with `outcome="rejected"` and `context.reason="GAUGE_COMPENSATION_REJECTED"` (HTTP `422` per the locked `usage-collector-v1.yaml` Problem.context.reason taxonomy and the SDK trait `GaugeCompensationRejected` variant) before any Plugin SPI dispatch — gauges have no `SUM` semantics, so `gauge` + `corrects_id SET` is the REJECTED cell of the four-cell value matrix and the locked five-code compensation taxonomy carves it out as its own enum (not collapsed into the generic `SEMANTICS_VIOLATION` code); no row is persisted (value-matrix enforcement).
- [ ] `p1` - A counter compensation emit (`corrects_id` set) whose `value` is greater than or equal to zero is rejected at validation time with `outcome="rejected"` and `context.reason="SEMANTICS_VIOLATION"` (`counter` + `corrects_id SET` requires `value < 0`; zero is not accepted as a no-op compensation); a counter ordinary-usage emit (`corrects_id IS NULL`) whose `value` is below zero remains rejected as before with the same `context.reason="SEMANTICS_VIOLATION"`; the unchanged `counter` + `corrects_id IS NULL` cell and the unchanged `gauge` + `corrects_id IS NULL` cell are both verifiable independently (four-cell value matrix).
- [ ] `p1` - A compensation emit (`corrects_id` set) whose `corrects_id` references a non-existent row, references a row that is itself a compensation (`corrects_id IS NOT NULL`), references a row from another tenant, references a row from another UsageType, references a different `resource_ref`, has a `subject_ref` presence or content mismatch, or references an `inactive` row is rejected at validation time via `cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2` with `outcome="rejected"` and a precise `context.reason` from the locked taxonomy before any Plugin SPI persist dispatch — `CORRECTS_ID_NOT_FOUND` (HTTP `404`) for non-existent references; `CORRECTS_ID_TARGETS_COMPENSATION` (HTTP `409`) for references whose target is itself a compensation; `CORRECTS_ID_WRONG_SCOPE` (HTTP `409`) for any mismatch on the `(tenant_id, gts_id, resource_ref, subject_ref)` identity tuple (`subject_ref` presence is part of the identity — `None` vs `Some(_)` is a scope error); `CORRECTS_ID_INACTIVE` (HTTP `409`) for `inactive` references (the L1 "referenced record MUST be `active`" branch also handles concurrent deactivation — a compensation referencing a row R that arrives while R is being deactivated surfaces the same `CORRECTS_ID_INACTIVE` code without quarantine or retry queue). The four granular `corrects_id_*` codes MUST NOT be collapsed into a single generic `corrects_id_invalid` code on the wire (L1 `corrects_id` enforcement and concurrency rule).
- [ ] `p1` - A compensation emit missing the mandatory caller-supplied `IdempotencyKey` is rejected at the wire level by the same `idempotency_key` NOT NULL requirement that applies to ordinary ingestion; an EXACT-EQUALITY retry of a previously-accepted compensation (same composite `(tenant_id, gts_id, idempotency_key)` and identical canonical fields including `value` and `corrects_id`) returns `outcome="accepted"` carrying the previously-persisted compensation body (silent absorb at the SPI — no second write, no double-refund effect); a same-key submission whose canonical fields differ surfaces `outcome="rejected"` with `context.reason="IDEMPOTENCY_CONFLICT"` (AlreadyExists/409) carrying the conflicting `idempotency_key` and `gts_id` (idempotency posture for compensation).
- [ ] `p1` - The Usage Collector MUST NOT compute refunds, credits, credit-notes, quota, lot/FIFO-LIFO state, or per-record remaining amounts for accepted compensations — the persisted row's `value`, `corrects_id`, and all other caller-supplied canonical fields are byte-identical to the request payload, and no L2 enforcement (non-negative net, negative-net detection / alerting / rejection) is performed by the Usage Collector; downstream consumers own any "net can't be negative" policy per the un-policed-net stance in `cpt-cf-usage-collector-adr-usage-compensation` (no-business-logic posture for compensation).
