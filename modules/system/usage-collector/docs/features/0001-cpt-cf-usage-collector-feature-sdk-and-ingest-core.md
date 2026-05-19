# Feature: Core SDK, Emitter & In-Process Ingest


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Usage Emission Flow](#usage-emission-flow)
  - [Module Config Retrieval Flow](#module-config-retrieval-flow)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Phase 1: `factory.with_*().authorize()` Authorization](#phase-1-factorywithauthorize-authorization)
  - [Phase 2: `usage_record_builder()?.build()? → enqueue(record)` — In-Transaction Enqueue](#phase-2-usage_record_builderbuild--enqueuerecord--in-transaction-enqueue)
  - [Outbox Delivery `MessageHandler`](#outbox-delivery-messagehandler)
  - [Gateway Ingest Handler](#gateway-ingest-handler)
  - [Static Module Config Resolution](#static-module-config-resolution)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [SDK Crate (`usage-collector-sdk`)](#sdk-crate-usage-collector-sdk)
  - [Emitter Crate (`usage-emitter`)](#emitter-crate-usage-emitter)
  - [Gateway Crate (`usage-collector`) — Ingest & Config](#gateway-crate-usage-collector--ingest--config)
  - [No-Op Plugin (`noop-usage-collector-plugin`)](#no-op-plugin-noop-usage-collector-plugin)
  - [Known Limitations / Technical Debt](#known-limitations--technical-debt)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Applicability Notes](#7-non-applicability-notes)

<!-- /toc -->

- [x] `p2` - **ID**: `cpt-cf-usage-collector-featstatus-sdk-and-ingest-core`
<!-- STATUS: IMPLEMENTED — all p1 DoD items and all CDSL blocks are [x]. -->

<!-- reference to DECOMPOSITION entry -->
- [x] `p1` - `cpt-cf-usage-collector-feature-sdk-and-ingest-core`
## 1. Feature Context

### 1.1 Overview

Establishes the core Usage Collector data model, SDK trait boundaries, three-layer Runtime/Factory/Emitter architecture with two-phase authorization, transactional outbox pipeline, gateway ingest handler, static metric configuration, and no-op storage plugin — delivering the complete in-process emission path from the factory's `.with_*().authorize(subject)` step through the outbox background pipeline to the gateway and plugin.

### 1.2 Purpose

Implements the foundation for all usage collection capabilities. Covers the SDK crate (`usage-collector-sdk`), the emitter crate (`usage-emitter`), the gateway crate (`usage-collector`) ingest and config endpoints, and the no-op storage plugin (`noop-usage-collector-plugin`). This feature is the prerequisite for all other Usage Collector features.

**Requirements**: `cpt-cf-usage-collector-fr-ingestion`, `cpt-cf-usage-collector-fr-idempotency`, `cpt-cf-usage-collector-fr-delivery-guarantee`, `cpt-cf-usage-collector-fr-counter-semantics`, `cpt-cf-usage-collector-fr-gauge-semantics`, `cpt-cf-usage-collector-fr-tenant-attribution`, `cpt-cf-usage-collector-fr-resource-attribution`, `cpt-cf-usage-collector-fr-subject-attribution`, `cpt-cf-usage-collector-fr-tenant-isolation`, `cpt-cf-usage-collector-fr-ingestion-authorization`, `cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-fr-record-metadata` (`p2`), `cpt-cf-usage-collector-nfr-availability`, `cpt-cf-usage-collector-nfr-ingestion-latency`, `cpt-cf-usage-collector-nfr-authentication`, `cpt-cf-usage-collector-nfr-authorization`, `cpt-cf-usage-collector-nfr-scalability`, `cpt-cf-usage-collector-nfr-fault-tolerance`, `cpt-cf-usage-collector-nfr-recovery`, `cpt-cf-usage-collector-nfr-graceful-degradation`, `cpt-cf-usage-collector-nfr-rpo`

**NFR targets (from PRD)**: `cpt-cf-usage-collector-nfr-ingestion-latency`
and `cpt-cf-usage-collector-nfr-availability` define numeric targets; values
are defined in PRD §NFRs and are not reproduced here. See PRD for
response-time and throughput targets.

**Principles**: `cpt-cf-usage-collector-principle-source-side-persistence`, `cpt-cf-usage-collector-principle-pluggable-storage`, `cpt-cf-usage-collector-principle-tenant-from-ctx`, `cpt-cf-usage-collector-principle-fail-closed`, `cpt-cf-usage-collector-principle-scoped-source-attribution`, `cpt-cf-usage-collector-principle-two-phase-authz`

### 1.3 Actors

**Actors** (defined in PRD.md):
- `cpt-cf-usage-collector-actor-usage-source` — initiates emission flows
- `cpt-cf-usage-collector-actor-platform-developer` — SDK integrator
- `cpt-cf-usage-collector-actor-storage-backend` — storage delegation target

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **DECOMPOSITION**: [DECOMPOSITION.md](../DECOMPOSITION.md)
- **Dependencies**: None

## 2. Actor Flows (CDSL)

### Usage Emission Flow

- [x] `p1` - **ID**: `cpt-cf-usage-collector-flow-sdk-and-ingest-core-emit`

**Actor**: `cpt-cf-usage-collector-actor-usage-source`

**Success Scenarios**:
- Record is durably enqueued in the source's local outbox within the caller's DB transaction
- Outbox background pipeline delivers the record to the gateway; plugin confirms storage

**Error Scenarios**:
- PDP denies `USAGE_RECORD`/`CREATE` → `UsageEmitterError::PermissionDenied` (built via `UsageRecordError::permission_denied()`); no outbox INSERT
- Module not configured → `UsageEmitterError::NotFound` (built via `ModuleConfigError::not_found()`); no outbox INSERT
- Metric not in allowed list → `UsageEmitterError::PermissionDenied` (built via `UsageRecordError::permission_denied()`); no outbox INSERT
- Counter record with negative value or missing idempotency key → `UsageEmitterError::InvalidArgument` (built via `UsageRecordError::invalid_argument()`); no outbox INSERT
- `UsageEmitter` handle exceeded max age → `UsageEmitterError::Unauthenticated` (built via `UsageEmitterError::unauthenticated()`); no outbox INSERT
- Metadata exceeds 8 KB → `UsageEmitterError::InvalidArgument` (built via `UsageRecordError::invalid_argument()`); no outbox INSERT
- Outbox delivery fails after retry budget exhausted → message moved to dead-letter store; surfaced via monitoring

**Steps**:
1. [x] - `p1` - Source retrieves `dyn UsageEmitterRuntimeV1` from `ClientHub` at module initialization - `inst-emit-1`
2. [x] - `p1` - Source calls `runtime.factory(MODULE_NAME)` to obtain a module-scoped `UsageEmitterFactory` (layer 2) bound to the source module's identity - `inst-emit-2`
3. [x] - `p1` - Before opening a DB transaction, source clones the factory, applies any per-call overrides via `with_tenant(...)` and the three-state subject setter chain — `with_subject(s)` (explicit caller-supplied) / `without_subject()` (explicit opt-out) / unset (`SubjectChoice::DefaultFromCtx` — resolve from `SecurityContext` at `.authorize()` time; the in-process default) — and runs the terminal `.authorize(ctx, resource_id, resource_type)` — triggers phase 1 authorization - `inst-emit-3`
4. [x] - `p1` - **IF** PDP denies or module is not configured - `inst-emit-4`
   1. [x] - `p1` - **RETURN** `UsageEmitterError`; no record is persisted - `inst-emit-4a`
5. [x] - `p1` - **RETURN** `UsageEmitter` (layer 3) carrying PDP permit, allowed-metrics list, and bound `tenant_id`/`resource_id`/`resource_type` - `inst-emit-5`
6. [x] - `p1` - Inside the source's DB transaction, source calls `UsageEmitter::usage_record_builder(metric, value)?.build()?` and passes the resulting `UsageRecord` to `UsageEmitter::enqueue(record)` / `enqueue_in(db, record)` — triggers phase 2 enqueue - `inst-emit-6`
7. [x] - `p1` - **IF** any in-memory validation fails (token expired, metric disallowed, counter invalid, metadata oversized) - `inst-emit-7`
   1. [x] - `p1` - **RETURN** `UsageEmitterError`; outbox INSERT is not executed - `inst-emit-7a`
8. [x] - `p1` - Outbox row is inserted into the source's local DB within the caller's transaction, serialized as `payload_type = "usage-collector.record.v1"` - `inst-emit-8`
9. [x] - `p1` - Outbox background pipeline picks up the row and calls `UsageCollectorClientV1::create_usage_record()` on delivery - `inst-emit-9`
10. [x] - `p1` - **IF** delivery fails transiently (network error, 5xx, 429) - `inst-emit-10`
    1. [x] - `p1` - Retry with exponential backoff; `outbox_backoff_max` MUST be configured below 15 minutes - `inst-emit-10a`
11. [x] - `p1` - **IF** delivery fails permanently (4xx excluding 429) - `inst-emit-11`
    1. [x] - `p1` - Move message to dead-letter store and surface via monitoring - `inst-emit-11a`
12. [x] - `p1` - **RETURN** delivery confirmed; record is available at the gateway - `inst-emit-12`

### Module Config Retrieval Flow

- [x] `p2` - **ID**: `cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config`

> _(p2: deferred — static module config reload requires gateway restart; implementing dynamic config reload is out of scope for Feature 1)_

**Actor**: `cpt-cf-usage-collector-actor-usage-source`

**Success Scenarios**:
- Gateway returns `ModuleConfig` with the static `allowed_metrics` list for the requesting module

**Error Scenarios**:
- Module not registered in static config → gateway returns 404; the factory's `.authorize()` step surfaces `UsageEmitterError::NotFound` (built via `ModuleConfigError::not_found()`)
- Transport failure or gateway 5xx → REST client returns `UsageEmitterError::ServiceUnavailable`; the factory's `.authorize()` step propagates it unchanged so the source module can surface it as HTTP 503
- Request deadline exceeded → REST client returns `UsageEmitterError::DeadlineExceeded`; the factory's `.authorize()` step propagates it unchanged so the source module can surface it as HTTP 504
- Gateway rate-limits the lookup (HTTP 429) → REST client returns `UsageEmitterError::ResourceExhausted`; the factory's `.authorize()` step propagates it unchanged
- Caller is rejected by the gateway (HTTP 403) → REST client returns `UsageEmitterError::PermissionDenied`; the factory's `.authorize()` step propagates it unchanged

**Steps**:
1. [x] - `p2` - During the factory's `.authorize()` step (phase 1), the emitter calls `UsageCollectorClientV1::get_module_config(module_name)` - `inst-cfg-1`
2. [x] - `p2` - Gateway receives `GET /usage-collector/v1/modules/{module_name}/config` authenticated via SecurityContext - `inst-cfg-2`
3. [x] - `p2` - Gateway looks up static metric configuration for the module - `inst-cfg-3`
4. [x] - `p2` - **IF** module not in static config - `inst-cfg-4`
   1. [x] - `p2` - **RETURN** 404; emitter surfaces `UsageEmitterError::NotFound` (built via `ModuleConfigError::not_found()`) - `inst-cfg-4a`
5. [x] - `p2` - **RETURN** `ModuleConfig { module_name, allowed_metrics: [AllowedMetric { name, kind }] }` - `inst-cfg-5`

## 3. Processes / Business Logic (CDSL)

### Phase 1: `factory.with_*().authorize()` Authorization

- [x] `p1` - **ID**: `cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for`

**Input**: `SecurityContext`, the factory's resolved `tenant: Option<Uuid>` (populated by `with_tenant(...)`) and `subject: SubjectChoice` (populated by `with_subject(...)` / `without_subject()`, otherwise the `DefaultFromCtx` default — see the subject resolution rule under step 1), `resource_id: Uuid`, `resource_type: String`

**Output**: `Result<UsageEmitter, UsageEmitterError>`

**Hot path**: The factory's `.authorize()` step (PDP call + config fetch) is the
latency-critical path for SDK callers. `get_module_config()` issues a REST
HTTP GET to the instance-configuration gateway (inst-cfg-2); the gateway
serves this from static in-memory configuration loaded at startup — no DB
I/O on the gateway side — so the per-call cost is network latency only, not
a DB round-trip. Operators should budget one synchronous HTTP round-trip per
`get_module_config()` call in the hot-path. Batch delivery and N+1 query
optimisation are not applicable — records are enqueued individually by
design.

**Steps**:
1. [x] - `p1` - Resolve `tenant_id` from `self.tenant` (falling back to `ctx.subject_tenant_id()` when `None`) and resolve `subject: Option<Subject>` from `self.subject` per the rule below; then call platform PDP: `USAGE_RECORD`/`CREATE`, passing `tenant_id`, `resource_id`/`resource_type` as resource properties, MODULE (the factory's immutable module name) as a resource property, and — **IF** the resolved `subject` is `Some(s)` — `SUBJECT_ID = s.id`, plus `SUBJECT_TYPE = s.r#type` when `s.r#type` is `Some` (each as a separate resource property; the PDP attribute schema is flat and unchanged); when `subject` is `None`, PDP subject properties are omitted from the request - `inst-authz-2`

   **Subject resolution rule** (matches ADR 0002 § "Subject handling"):

   ```rust
   let subject = match &self.subject {
       SubjectChoice::Explicit(s) => s.clone(),
       SubjectChoice::DefaultFromCtx => Some(subject_from_ctx(ctx)),
   };
   ```

   — The factory's `self.subject` is a `SubjectChoice` enum with three reachable states: `DefaultFromCtx` (neither `.with_subject(...)` nor `.without_subject()` was called) falls back to deriving the subject from `SecurityContext`; `Explicit(Some(s))` (set by `.with_subject(s)`) is used as-is; `Explicit(None)` (set by `.without_subject()`) is the explicit "no subject" choice, sending no `SUBJECT_ID`/`SUBJECT_TYPE` to the PDP. The REST handler / forwarders translate their wire-level `Option<Subject>` faithfully — `Some(s) → .with_subject(s)`, `None → .without_subject()` — so the forwarder's own service identity is never substituted for an absent caller-supplied subject.

2. [x] - `p1` - **IF** PDP denies - `inst-authz-3`
   1. [x] - `p1` - **RETURN** `UsageEmitterError::PermissionDenied` (built via `UsageRecordError::permission_denied()`) - `inst-authz-3a`
3. [x] - `p1` - Call `get_module_config(module_name)` to fetch `AllowedMetric` list from gateway - `inst-authz-4`
4. [x] - `p1` - **IF** `get_module_config` returns an error - `inst-authz-5`
   1. [x] - `p1` - **RETURN** the `UsageEmitterError` unchanged so the canonical variant chosen by the collector (or REST client) flows end-to-end: `NotFound` (built via `ModuleConfigError::not_found()`) when the module is not in static config; `DeadlineExceeded`, `ServiceUnavailable`, `ResourceExhausted`, `PermissionDenied`, or `Internal` for transport / gateway / parse failures. The emitter MUST NOT collapse non-`NotFound` variants into `Internal` — the categorization is the contract source modules use to map a transient gateway outage to HTTP 503/504 (or rate-limit to 429) instead of an opaque 500. - `inst-authz-5a`
5. [x] - `p1` - Bind PDP permit result, allowed-metrics list, `tenant_id`, `resource_id`, `resource_type`, resolved `subject: Option<Subject>`, and issuance timestamp into the returned `UsageEmitter` (layer 3) - `inst-authz-6`
6. [x] - `p1` - **RETURN** `UsageEmitter` - `inst-authz-7`

### Phase 2: `usage_record_builder()?.build()? → enqueue(record)` — In-Transaction Enqueue

- [x] `p1` - **ID**: `cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue`

**Input**: `UsageEmitter`, metric name, value, optional idempotency key, optional metadata JSON

**Output**: `Result<(), UsageEmitterError>`

**Optional field serialization**: `metadata` is optional. When absent it serializes as an absent JSON field (not `null`). Deserialization treats absent as `None` with no default substitution. `idempotency_key` is optional from the caller's perspective but is **always present in the serialized record** — when the caller omits it, a UUID v4 is auto-generated before enqueue so the wire format always carries a non-null key. Blank strings (`""` or whitespace-only) are semantically equivalent to `None` for this field and MUST NOT be stored as a valid key.

**Entry points**: callers obtain a `UsageRecord` from the dumb `UsageRecordBuilder` (most often via `UsageEmitter::usage_record_builder(metric, value)?` which prefills the authorized fields and resolves the metric kind from the allowed-metrics list, then `.build()?`) and pass it to one of two equivalent terminal operations on `UsageEmitter`. `enqueue(record)` resolves a pooled connection via the source's `modkit_db::Db` handle (the `DBRunner` provider obtained at runtime construction) and then runs the same algorithm as `enqueue_in(db, record)`; this is the convenience path for callers that do not already hold a transaction. `enqueue_in(db: &(dyn DBRunner + Sync), record)` accepts a caller-supplied `DBRunner` (a pooled connection, a borrowed connection, or an in-flight transaction handle) and runs the outbox INSERT against that handle so the record is enqueued *inside the caller's open transaction*. `enqueue_in` is the canonical transactional-outbox entry point — when the caller passes a transaction handle, the outbox INSERT either commits with the rest of the caller's writes or is rolled back atomically with them. The two entry points share `inst-enq-1` through `inst-enq-10` verbatim; only the source of the `DBRunner` differs.

**Steps**:
1. [x] - `p1` - Verify the `UsageEmitter` handle has not exceeded its maximum age - `inst-enq-1`
2. [x] - `p1` - **IF** the handle is expired - `inst-enq-2`
   1. [x] - `p1` - **RETURN** `UsageEmitterError::Unauthenticated` (built via `UsageEmitterError::unauthenticated()` with reason "emit authorization has expired") - `inst-enq-2a`
3. [x] - `p1` - Verify metric name is present in the handle's allowed-metrics list - `inst-enq-3`
4. [x] - `p1` - **IF** metric not in allowed list - `inst-enq-4`
   1. [x] - `p1` - **RETURN** `UsageEmitterError::PermissionDenied` (built via `UsageRecordError::permission_denied()` with reason "metric not allowed for this module") - `inst-enq-4a`
5. [x] - `p1` - **IF** metric kind is `counter` AND (value < 0 OR idempotency_key is None); an empty or whitespace-only string MUST be treated as absent (equivalent to `None`) - `inst-enq-5`
   1. [x] - `p1` - **RETURN** `UsageEmitterError::InvalidArgument` (built via `UsageRecordError::invalid_argument()`) - `inst-enq-5a`
5a. [x] - `p1` - **IF** idempotency_key is None (gauge record without caller-supplied key) — generate a UUID v4 and assign it as the idempotency_key; an empty or whitespace-only string MUST be treated as absent and triggers the UUID fallback - `inst-enq-5b`
6. [x] - `p1` - Validate `record.module` equals the handle's bound module name; if mismatch RETURN `UsageEmitterError::PermissionDenied` (built via `UsageRecordError::permission_denied()` with reason "record module does not match authorized emitter"). Validate `record.subject == self.subject` (structural equality on `Option<Subject>`); if mismatch RETURN `UsageEmitterError::PermissionDenied` (built via `UsageRecordError::permission_denied()` with reason "record subject does not match authorized token"). The "type without id" payload is not expressible, so subject mismatch is a single structural comparison. - `inst-enq-6`
7. [x] - `p1` - **IF** metadata is present AND byte length > `self.max_metadata_bytes` (sourced from `ModuleConfig.max_metadata_bytes` at `.authorize()` time; value 0 disables metadata entirely so any non-None payload is rejected) - `inst-enq-7`
   1. [x] - `p1` - **RETURN** `UsageEmitterError::InvalidArgument` (built via `UsageRecordError::invalid_argument()` with constraint "metadata byte length exceeds the configured `max_metadata_bytes` limit") - `inst-enq-7a`
8. [x] - `p1` - Serialize `UsageRecord` (tenant_id, module, kind, metric, value, idempotency_key, resource_id, resource_type, `subject: Option<Subject>`, metadata, timestamp) with `payload_type = "usage-collector.record.v1"`; `subject` and `metadata` serialize as absent JSON fields when `None` (not as `null`). When `subject` is `Some`, it is emitted as a nested object: `"subject": { "id": "...", "type": "user" }` (the inner `type` is itself absent when `Subject.r#type` is `None`). - `inst-enq-8`
9. [x] - `p1` - Call `Outbox::enqueue(db, payload, payload_type)` against the `DBRunner` resolved by the entry point — for `enqueue_in(db)` this is the caller's connection or transaction handle; for `enqueue()` it is a pooled connection acquired from the source's configured `DBRunner` provider; the outbox INSERT therefore participates in the caller's active transaction whenever one was supplied - `inst-enq-9`
10. [x] - `p1` - **RETURN** `Ok(())`; record is durably enqueued and delivery proceeds asynchronously - `inst-enq-10`

### Outbox Delivery `MessageHandler`

- [x] `p1` - **ID**: `cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery`

**Input**: Serialized outbox message with `payload_type = "usage-collector.record.v1"`

**Output**: `HandlerResult`

**Queue overflow**: The outbox grows as long as the storage plugin is unavailable. No max-rows limit is enforced by this feature — unbounded growth is an operational concern delegated to DB capacity management. `enqueue()` does not apply backpressure to the caller; callers experience DB write latency only. Operators should monitor outbox queue depth (see Observability) and provision DB capacity accordingly.

**Message ordering**: Ordering across the 4 outbox partitions is not guaranteed. Per-partition ordering may be preserved by the `modkit-db` outbox library but is not relied upon by this feature. Idempotency keys on counter records provide at-least-once deduplication at the storage layer.

**Steps**:
1. [x] - `p1` - Deserialize outbox payload bytes into `UsageRecord` - `inst-dlv-1`
2. [x] - `p1` - **IF** deserialization fails - `inst-dlv-2`
   1. [x] - `p1` - **RETURN** `HandlerResult::Reject`; unrecoverable format error — message moved to dead-letter store - `inst-dlv-2a`
3. [x] - `p1` - Assemble gateway ingest request from `UsageRecord` fields - `inst-dlv-3`
4. [x] - `p1` - Call `UsageCollectorClientV1::create_usage_record(record)` - `inst-dlv-4`
5. [x] - `p1` - **IF** call succeeds (204 No Content) - `inst-dlv-5`
   1. [x] - `p1` - **RETURN** `HandlerResult::Success`; outbox row is deleted - `inst-dlv-5a`
6. [x] - `p1` - **IF** the canonical error is canonically retryable — one of `UsageCollectorError::DeadlineExceeded` (network/server timeout), `UsageCollectorError::ResourceExhausted` (HTTP 429 rate limit), `UsageCollectorError::ServiceUnavailable` (connection/transport error, AuthN service temporarily unreachable, circuit breaker open, plugin not ready), `UsageCollectorError::Cancelled` (caller cancellation, recoverable on a later attempt), or `UsageCollectorError::Aborted` (concurrency conflict on the receiver) - `inst-dlv-6`
   1. [x] - `p1` - **RETURN** `HandlerResult::Retry`; outbox library applies exponential backoff; `outbox_backoff_max` MUST be configured below 15 minutes to satisfy `cpt-cf-usage-collector-nfr-recovery` - `inst-dlv-6a`
7. [x] - `p1` - **IF** the canonical error is permanent — caller-induced variants (`InvalidArgument`, `Unauthenticated`, `PermissionDenied`, `NotFound`, `AlreadyExists`, `FailedPrecondition`, `OutOfRange`, `Unimplemented`) **OR** the gRPC-canonically permanent variants `Internal`, `Unknown`, `DataLoss` (serious defects, unrecognized error space, or unrecoverable corruption — none of which improve with retry; real transient infra conditions surface as `ServiceUnavailable` per the gateway's `DomainError → UsageCollectorError` translation) - `inst-dlv-7`
   1. [x] - `p1` - **RETURN** `HandlerResult::Reject`; message moved to dead-letter store and surfaced via monitoring for operator inspection - `inst-dlv-7a`

### Gateway Ingest Handler

- [x] `p1` - **ID**: `cpt-cf-usage-collector-algo-sdk-and-ingest-core-gateway-ingest-handler`

**Input**: `UsageRecord` delivered by the outbox pipeline, `SecurityContext`

**Output**: 204 No Content or error response

**Steps**:
1. [x] - `p1` - Enforce no per-record metadata size limit at the gateway — the limit is enforced upstream by the emitter using `ModuleConfig.max_metadata_bytes`; the gateway only publishes this value via `get_module_config` - `inst-gw-1`
2. [x] - `p1` - Check circuit breaker state for the active plugin instance - `inst-gw-2`
3. [x] - `p1` - **IF** circuit is open **OR** circuit is in half-open state with a probe already in-flight - `inst-gw-3`
   1. [x] - `p1` - **RETURN** `503 Service Unavailable` - `inst-gw-3a`
4. [x] - `p1` - Resolve the active storage plugin via GTS - `inst-gw-4`
5. [x] - `p1` - Call `plugin.create_usage_record(record)` with configurable timeout (default 5 s) - `inst-gw-5`
6. [x] - `p1` - **IF** plugin call times out or fails transiently - `inst-gw-6`
   1. [x] - `p1` - Record failure against circuit breaker; open circuit after 5 consecutive failures within a 10 s window; half-open probe after configurable interval (default 30 s) - `inst-gw-6a`
   2. [x] - `p1` - **RETURN** transient error; retry is handled by the outbox library on the SDK side - `inst-gw-6b`
7. [x] - `p1` - **RETURN** 204 No Content on successful plugin confirmation - `inst-gw-7`

### Static Module Config Resolution

- [x] `p2` - **ID**: `cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config`

> _(p2: deferred — static module config reload requires gateway restart; implementing dynamic config reload is out of scope for Feature 1)_

**Input**: `module_name: String`, `SecurityContext`

**Output**: `Result<ModuleConfig, ModuleConfigError>`

**Cache integration**: Not applicable — `ModuleConfig` is loaded from static gateway
configuration at startup. No runtime caching layer is introduced in this feature.

**Steps**:
1. [x] - `p2` - Authenticate request via SecurityContext; ModKit pipeline rejects unauthenticated requests before the handler - `inst-cfg-p-1`
2. [x] - `p2` - Look up module name in the gateway's static `metrics` configuration - `inst-cfg-p-2`
3. [x] - `p2` - **IF** module not found in static config - `inst-cfg-p-3`
   1. [x] - `p2` - **RETURN** 404 Not Found - `inst-cfg-p-3a`
4. [x] - `p2` - **RETURN** `ModuleConfig { module_name, allowed_metrics }` - `inst-cfg-p-4`

## 4. States (CDSL)

Not applicable for this feature. `UsageRecord.status` transitions (`active` → `inactive`) are owned by Feature 8 (operator amendment and deactivation). Outbox message lifecycle is managed by the `modkit-db` outbox library and is not a domain state machine defined here.

## 5. Definitions of Done

### SDK Crate (`usage-collector-sdk`)

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-sdk-and-ingest-core-sdk-crate`

The system **MUST** implement the `usage-collector-sdk` crate providing the following public surface:

- **Delivery trait**: `UsageCollectorClientV1` (`create_usage_record()`, `get_module_config()`) — passed by constructor argument to the emitter, never registered in `ClientHub`.
- **Plugin trait**: `UsageCollectorPluginClientV1` (`create_usage_record()` write operation; `query_aggregated()` and `query_raw()` read operations are added by Feature 3 and the gateway compiles the PDP-derived `AccessScope` into `AggregationQuery.scope` / `RawQuery.scope`, so the plugin contract takes no separate `SecurityContext`).
- **Ingest-side model types**: `UsageRecord`, `UsageKind`, `ModuleConfig`, `AllowedMetric`.
- **Query-side model types** (re-exported for plugin and gateway implementors; defined in detail by Feature 3): `AggregationFn`, `BucketSize`, `GroupByDimension`, `AggregationQuery`, `AggregationResult`, `RawQuery`.
- **Pagination types** (re-exported from `modkit-odata`): `CursorV1`, `Page`, `PageInfo`.
- **Error taxonomy**: `UsageCollectorError = CanonicalError` (re-exported from `modkit-canonical-errors`), plus the resource-scoped error builders `UsageRecordError` (GTS prefix `gts.cf.core.usage.record.v1~`) and `ModuleConfigError` (GTS prefix `gts.cf.core.usage.module_config.v1~`) — these builders are the canonical error-construction API used by the emitter, gateway, REST client, and storage plugins.
- **PDP authorization constants**: an `authz` module exporting `USAGE_RECORD: ResourceType` (with `supported_properties` covering `OWNER_TENANT_ID`, `RESOURCE_ID`, `RESOURCE_TYPE`, `MODULE`, `SUBJECT_ID`, `SUBJECT_TYPE`), the `actions::{CREATE, LIST}` constants, and the `properties::{RESOURCE_ID, RESOURCE_TYPE, MODULE, SUBJECT_ID, SUBJECT_TYPE}` property-name constants. Used by the emitter (`CREATE`) and the collector gateway (`LIST`) to call the platform PDP with a consistent attribute surface.
- **GTS schema**: `UsageCollectorPluginSpecV1` for storage-plugin registration.

**Implements**:
- `cpt-cf-usage-collector-component-sdk`

**Constraints**: `cpt-cf-usage-collector-constraint-modkit`

**Touches**:
- Entities: `UsageRecord`, `ModuleConfig`, `AllowedMetric`, `UsageKind`

**Data Protection**: `UsageRecord` fields (`tenant_id`, `resource_id`, `resource_type`,
`subject.id`, `subject.r#type`) are classified as internal billing identifiers —
opaque UUIDs and string identifiers — not PII under the project's data
classification policy. `subject` is `Option<Subject>` and the inner `Subject.r#type` is itself optional; when `subject` is absent from a record, no subject attribution is stored. Data minimization: only fields required for billing
attribution are collected. Data subject deletion rights: not applicable at the
feature level; delegated to the storage plugin (Feature 4). Encryption at rest
and in transit: not enforced by this feature; delegated to the storage plugin
and its infrastructure (Feature 4 — Production Storage Plugin).

### Emitter Crate (`usage-emitter`)

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-sdk-and-ingest-core-emitter-crate`

The system **MUST** implement the `usage-emitter` crate providing the three-layer Runtime / Factory / Emitter architecture: `UsageEmitterRuntime` (concrete, registered as `dyn UsageEmitterRuntimeV1` in `ClientHub`) exposing `factory(module_name) -> UsageEmitterFactory`; `UsageEmitterFactory::with_tenant(Uuid)`, `with_subject(Subject)`, and `without_subject()` chainable scope overrides terminating in `.authorize(ctx, resource_id, resource_type) -> Result<UsageEmitter, UsageEmitterError>` (PDP call + module config fetch; tenant defaults to `ctx.subject_tenant_id()` when `with_tenant(...)` is not called; subject is resolved at `.authorize()` time via `match &self.subject { SubjectChoice::Explicit(s) => s.clone(), SubjectChoice::DefaultFromCtx => Some(subject_from_ctx(ctx)) }` — see the three-state resolution rule in the algorithm above); `UsageEmitter::usage_record_builder(metric, value)` (returns a `UsageRecordBuilder` prefilled from the authorized handle; resolves metric kind from the allowed-metrics list and returns `PermissionDenied` for unknown metrics), `UsageRecordBuilder::build()` (assembles a `UsageRecord`; returns `InvalidArgument` listing any missing required fields), `UsageEmitter::enqueue(record)` (all in-memory validations + `Outbox::enqueue()` against a pooled connection resolved from the source's `modkit_db::Db` handle) and the equivalent `.enqueue_in(db: &(dyn DBRunner + Sync), record)` overload that runs the same algorithm against a caller-supplied `DBRunner` so the outbox INSERT participates in the caller's open transaction (the canonical transactional-outbox path), the outbox `MessageHandler` with `outbox_backoff_max` configured below 15 minutes, and registration of `UsageEmitterRuntimeV1` in `ClientHub` during gateway `init()`.

The crate is laid out as follows under `usage-emitter/src/`:

- `api.rs` — defines the public `UsageEmitterRuntimeV1` trait (the `ClientHub` registration key consumed by source modules and by `usage-collector-rest-client`).
- `config.rs` — `UsageEmitterConfig` (authorization age, outbox backoff bounds).
- `error.rs` — `UsageEmitterError = CanonicalError`; the crate-specific enum is no longer exposed (canonical taxonomy aligned per v1.11 changelog).
- `domain/runtime.rs` — `UsageEmitterRuntime` (layer 1): concrete struct holding the outbox worker (`OutboxHandle` for process-lifetime task ownership), gateway client, PDP resolver, shared `Arc`s, and config. Exposes the async constructor `UsageEmitterRuntime::build(config, db, authz, collector)` (this is where config validation lives) and implements `UsageEmitterRuntimeV1::factory(module_name) -> UsageEmitterFactory`. One runtime instance is registered per process by the gateway / REST-client modules.
- `domain/factory.rs` — `UsageEmitterFactory` (layer 2): cloneable, module-scoped struct holding the shared `Arc`s cloned from the runtime, the immutable `module: String` (set at construction by `runtime.factory(name)` — there is no `with_module(...)` setter), and the overridable scope (`tenant: Option<Uuid>`, `subject: SubjectChoice` — a crate-private enum with three reachable states: `DefaultFromCtx` (the runtime default — fall back to `SecurityContext` at `.authorize()` time), `Explicit(Some(s))` (set by `.with_subject(s)`), `Explicit(None)` (set by `.without_subject()`)). Exposes `with_tenant`, `with_subject`, `without_subject`, and the terminal `authorize`. Also contains the free function `fn subject_from_ctx(ctx: &SecurityContext) -> Subject` used as the in-process-default fallback; this is a free function (not a `From` impl) because Rust orphan rules forbid an `impl From<&SecurityContext> for Subject` when both types are foreign to this crate.
- `domain/emitter.rs` — `UsageEmitter` (layer 3): per-call-site authorized handle returned by `UsageEmitterFactory::authorize(...)`. Carries the `tenant_id`, `resource_id`, `resource_type`, bound `subject: Option<Subject>`, allowed-metrics list, `issued_at: Instant`, and the shared outbox/db handles. The redundant `Authorized` qualifier from the legacy name is removed: holding a `UsageEmitter` *is* the proof of authorization.
- `domain/usage_record_builder.rs` — `UsageRecordBuilder`, a standalone "dumb" builder with per-field setters (`with_module`, `with_tenant_id`, `with_metric(name, kind)`, `with_value`, `with_resource(id, type)`, `with_subject(Subject)`, `with_idempotency_key`, `with_timestamp`, `with_metadata`). Constructed via `UsageRecordBuilder::new()` for standalone use or via `UsageEmitter::usage_record_builder(metric, value)?` which prefills `module`, `tenant_id`, `resource_id` / `resource_type`, `subject: Option<Subject>`, `metric` (with `kind` resolved from the authorized allowed-metrics list — unknown metrics return `PermissionDenied` here), and `value`. `UsageRecordBuilder::build()` assembles a `UsageRecord` and returns `UsageEmitterError::InvalidArgument` enumerating any missing required field; it performs no PDP / metric-allowed-list / metadata validation. The resulting `UsageRecord` is then passed to `UsageEmitter::enqueue(record)` (pooled connection from the source's `modkit_db::Db` handle — the convenience path) or `UsageEmitter::enqueue_in(db: &(dyn DBRunner + Sync), record)` (caller-supplied connection or transaction handle — the canonical "transactional outbox within caller's transaction" path).
- `infra/delivery_handler.rs` — `DeliveryHandler` implementing the outbox `MessageHandler`: deserializes outbox payloads, calls `UsageCollectorClientV1::create_usage_record()`, and routes `DeadlineExceeded` / `ResourceExhausted` / `ServiceUnavailable` to `HandlerResult::Retry`, everything else to `HandlerResult::Reject`.

The crate's public surface (`lib.rs`) is exactly `pub use api::UsageEmitterRuntimeV1; pub use config::UsageEmitterConfig; pub use domain::emitter::UsageEmitter; pub use domain::factory::UsageEmitterFactory; pub use domain::runtime::UsageEmitterRuntime; pub use domain::usage_record_builder::UsageRecordBuilder; pub use error::UsageEmitterError; pub use infra::delivery_handler::DeliveryHandler;`.

**Implements**:
- `cpt-cf-usage-collector-flow-sdk-and-ingest-core-emit`
- `cpt-cf-usage-collector-algo-sdk-and-ingest-core-authorize-for`
- `cpt-cf-usage-collector-algo-sdk-and-ingest-core-enqueue`
- `cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery`
- `cpt-cf-usage-collector-component-emitter`

**Constraints**: `cpt-cf-usage-collector-constraint-outbox-infra`, `cpt-cf-usage-collector-constraint-security-context`, `cpt-cf-usage-collector-constraint-modkit`

**Touches**:
- DB: `outbox` (source's local DB, `cpt-cf-usage-collector-dbtable-outbox`)
- Entities: `UsageRecord`, `UsageEmitter`

**Audit logging**: Calls to the factory's `.authorize()` step are not individually audited at
the SDK boundary — audit of usage-collector ingestion calls is delegated to
the platform-wide API audit layer. Calls to `POST /usage-collector/v1/records`
are similarly delegated. No feature-level audit log is produced.

**Resource management**: `UsageEmitter` handles may be reused for
multiple `enqueue()` calls within `authorization_max_age` (default 30 s);
freshness is time-based, not single-use. Handles are dropped when the owning
scope exits — no additional cleanup required. DB connection
pooling for the outbox is fully managed by the `modkit-db` outbox library;
this feature does not hold connections. `UsageCollectorClientV1` connection
pool is managed by the ModKit HTTP client; no feature-owned connection
lifecycle.

**Concurrency**: The factory's `.authorize()` step and `UsageEmitter::enqueue()` are safe for concurrent
calls — all state is per-call with no shared mutable state across factory clones / emitter handles.
Rate limiting on `POST /usage-collector/v1/records` is not in scope for
this feature; it is delegated to the platform API gateway.

**Observability**: Structured log events MUST be emitted for: authorization
denial (`WARN`), validation failure (`WARN`), delivery retry (`INFO`),
dead-letter routing (`ERROR`), circuit breaker state transitions
(`WARN`/`INFO`). Metrics: outbox queue depth, delivery attempt count,
plugin call latency (histogram), circuit breaker open/closed state (gauge).
OpenTelemetry trace propagation across the outbox pipeline boundary is
deferred to a future observability feature; correlation IDs from inbound
requests are not propagated in this feature.

**Data access patterns**: Not applicable — DB access is fully mediated by the `modkit-db` outbox library. This feature constructs no raw queries; index usage, join patterns, and aggregation patterns are encapsulated by the library.

**Data archival and retention**: Not applicable to this feature. Archival and retention compliance are delegated to the storage plugin implementation and its backing infrastructure. The outbox schema migration is forward-only via `DatabaseCapability::migrations()`; schema rollback is not supported.

**Connection management**: Not applicable — connection management, query parameterization, and
result handling are fully encapsulated by the `modkit-db` outbox library. This feature constructs
no raw queries.

### Gateway Crate (`usage-collector`) — Ingest & Config

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-sdk-and-ingest-core-gateway-crate`

The system **MUST** implement in the `usage-collector` gateway crate: outbox queue registration (`"usage-records"`, 4 partitions, configurable) and schema migrations via `DatabaseCapability::migrations()`, `POST /usage-collector/v1/records` ingest handler (GTS plugin resolution with timeout, circuit breaker — 5 failures / 10 s open, 30 s half-open probe), `GET /usage-collector/v1/modules/{module_name}/config` handler (static metric config lookup; exposes `max_metadata_bytes` alongside the allowed-metrics list so emitters share the gateway's policy), and construction + registration of `UsageEmitterRuntimeV1` (backed by `UsageCollectorLocalClient`) in `ClientHub` during `init()`.

The crate's domain layer is structured as the following modules under `src/domain/`:

- `service.rs` — the `Service` orchestrator that fronts the ingest, query, and module-config code paths; resolves the active storage plugin via GTS, enforces per-call timeouts, and routes plugin invocations through the circuit breaker. Each plugin call propagates a `SecurityContext` and (for queries) a pre-compiled `AccessScope`.
- `local_client.rs` — `UsageCollectorLocalClient` implementing `usage_collector_sdk::UsageCollectorClientV1` for in-process delivery from the gateway's own outbox `MessageHandler`. Translates `DomainError` to `UsageCollectorError` at the SDK boundary so in-process and remote callers see the same canonical error taxonomy.
- `authz.rs` — gateway-side PDP enforcement for the query API (`USAGE_RECORD` / `LIST` action) and for any other operation that needs constraint compilation; produces the `AccessScope` that `Service` embeds in `AggregationQuery.scope` / `RawQuery.scope`. Ingest does **not** call this module — tenant attribution is authorized at emit time by `usage-emitter`.
- `circuit_breaker.rs` — the per-plugin sliding-window breaker (5 failures / 10 s window opens the circuit; one half-open probe is admitted after the configurable recovery interval, default 30 s; everything else is rejected as `DomainError::CircuitOpen`). Failure classification differs by state. In `Closed` state the breaker's failure classifier `is_health_failure` treats the following as plugin/infrastructure ill-health and counts them toward the failure window: `DomainError::TypesRegistryUnavailable`, `DomainError::PluginNotFound`, `DomainError::PluginUnavailable`, `DomainError::Timeout`, `DomainError::Internal`, plus `DomainError::Plugin(canonical)` whose canonical variant is one of `UsageCollectorError::ServiceUnavailable`, `UsageCollectorError::Internal`, `UsageCollectorError::Unknown`, `UsageCollectorError::DataLoss`, or `UsageCollectorError::DeadlineExceeded`. All other variants — including `DomainError::CircuitOpen` (already-open, not a new failure signal), `DomainError::InvalidPluginInstance`, `DomainError::ModuleNotConfigured`, and any caller-induced `CanonicalError` (`InvalidArgument`, `NotFound`, `PermissionDenied`, `Unauthenticated`, `ResourceExhausted`, `FailedPrecondition`, `Aborted`, `OutOfRange`, `AlreadyExists`, `Cancelled`) — do **not** trip the breaker in `Closed` state. The classifier rules above apply only to `Closed` traffic. In `HalfOpen` state the probe is strict: any non-success outcome — including caller-induced `CanonicalError` that would be ignored in `Closed` — re-opens the circuit irrespective of `is_health_failure`. Only a successful probe transitions the breaker back to `Closed`.
- `error.rs` — the gateway-internal `DomainError` enum (`TypesRegistryUnavailable`, `PluginNotFound`, `InvalidPluginInstance`, `PluginUnavailable`, `Timeout`, `CircuitOpen`, `ModuleNotConfigured`, `Plugin(UsageCollectorError)`, `Internal`) and its `From<DomainError> for UsageCollectorError` translation (`Plugin` passes through; `ModuleNotConfigured` becomes `ModuleConfigError::not_found(...).with_resource(module).create()`; `Timeout` becomes `UsageRecordError::deadline_exceeded(...).create()`; `CircuitOpen` / `PluginNotFound` / `PluginUnavailable` become `UsageCollectorError::service_unavailable()` with the appropriate detail; `InvalidPluginInstance`, `TypesRegistryUnavailable`, and `Internal` become `UsageCollectorError::internal(...)`).

**Implements**:
- `cpt-cf-usage-collector-flow-sdk-and-ingest-core-fetch-module-config`
- `cpt-cf-usage-collector-algo-sdk-and-ingest-core-gateway-ingest-handler`
- `cpt-cf-usage-collector-algo-sdk-and-ingest-core-get-module-config`
- `cpt-cf-usage-collector-component-gateway`

**Constraints**: `cpt-cf-usage-collector-constraint-outbox-infra`, `cpt-cf-usage-collector-constraint-single-plugin`, `cpt-cf-usage-collector-constraint-modkit`, `cpt-cf-usage-collector-constraint-security-context`, `cpt-cf-usage-collector-constraint-no-business-logic`

**Touches**:
- API: `POST /usage-collector/v1/records`, `GET /usage-collector/v1/modules/{module_name}/config`
- DB: `outbox` (`"usage-records"` queue, `cpt-cf-usage-collector-dbtable-outbox`)
- Entities: `UsageRecord`, `ModuleConfig`

**Audit logging**: Calls to the factory's `.authorize()` step are not individually audited at
the SDK boundary — audit of usage-collector ingestion calls is delegated to
the platform-wide API audit layer. Calls to `POST /usage-collector/v1/records`
are similarly delegated. No feature-level audit log is produced.

**Security error handling**: The gateway strips internal stack traces before
returning 4xx/5xx responses to callers. The `.authorize()` step's timing: constant-time
response patterns are not applied at the SDK layer; tenant-existence enumeration
via PDP call timing is mitigated by the PDP's own response-time guarantees.
Rate limiting on `.authorize()` calls is out of scope for this feature —
delegated to the platform gateway rate-limiting layer.

**Observability**: Structured log events MUST be emitted for: authorization
denial (`WARN`), validation failure (`WARN`), delivery retry (`INFO`),
dead-letter routing (`ERROR`), circuit breaker state transitions
(`WARN`/`INFO`). Metrics: outbox queue depth, delivery attempt count,
plugin call latency (histogram), circuit breaker open/closed state (gauge).
OpenTelemetry trace propagation across the outbox pipeline boundary is
deferred to a future observability feature; correlation IDs from inbound
requests are not propagated in this feature.

**Rollout/rollback**: The outbox schema migration
(`DatabaseCapability::migrations()`) is forward-only — rollback of the
schema is not supported. Rollback of the gateway binary to a pre-feature
version is safe only if no messages have been enqueued; enqueued rows will
remain in the DB until the migrated gateway is redeployed. No feature flag
guards the new endpoints in this feature — rollout strategy is managed at
the platform level via standard deployment controls.

**Recovery**: In-flight outbox messages survive a gateway upgrade — rows are durable in the DB and will be picked up by the restarted process. Circuit breaker state and plugin registration are recovered automatically on gateway restart (stateless configuration). Dead-lettered records: operators inspect via direct DB query on the dead-letter partition; reprocessing requires manual row deletion and re-insertion into the live queue, or a future admin API (out of scope). No compensating transaction is required for the delivery pipeline.

**Data access patterns**: Not applicable — all storage I/O is delegated to the active storage plugin via `UsageCollectorPluginClientV1`. The gateway constructs no raw DB queries; plugin selection, connection pooling, and query execution are fully encapsulated by the plugin implementation.

**Data archival and retention**: Not applicable to this feature. Archival and retention compliance are delegated to the storage plugin implementation and its backing infrastructure. The outbox schema migration is forward-only via `DatabaseCapability::migrations()`; schema rollback is not supported.

**Configuration**:

| Parameter | Type | Valid range | Default | Validation | Runtime-changeable |
|-----------|------|-------------|---------|------------|--------------------|
| `outbox_backoff_max` | duration | 1s–15m | 600s (10 min) | must be > 0 and < 900s | No — requires restart |
| Plugin timeout | duration | 100ms–30s | 5s | must be > 0 | No |
| Circuit breaker failure threshold | integer | 1–100 | 5 | must be ≥ 1 | No |
| Circuit breaker recovery timeout | duration | 1s–5m | 30s | must be > 0 | No |
| Queue partitions | integer | 1–64 | 4 | must be ≥ 1 | No |

No feature flags are used; all configuration is static and requires gateway restart to change.

**Health & diagnostics**: Circuit breaker state is not directly exposed via `GET /health` in this feature — it contributes to the platform-level health aggregate. Outbox queue depth is a recommended monitoring metric (see Observability). First-level troubleshooting: (1) check circuit breaker state via structured logs; (2) inspect outbox queue depth; (3) verify storage plugin connectivity; (4) check dead-letter partition for accumulated records.

`cpt-cf-usage-collector-constraint-encryption` is not enforced by this feature — encryption at
rest and in transit is deferred to Feature 4 (Production Storage Plugin) which owns the
production storage backend.

### No-Op Plugin (`noop-usage-collector-plugin`)

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-sdk-and-ingest-core-noop-plugin`

> _(p2: deferred — noop plugin validation is a test-time concern; plugin interface is validated by integration tests)_

The system **MUST** implement the `noop-usage-collector-plugin` crate providing a no-op implementation of `UsageCollectorPluginClientV1` where all write operations succeed without persisting data and all read operations return empty results. Must register via `UsageCollectorPluginSpecV1` GTS schema for selection by operator configuration in test and local-dev deployments.

**Implements**:
- `cpt-cf-usage-collector-component-storage-plugin` (no-op only)

**Constraints**: `cpt-cf-usage-collector-constraint-single-plugin`, `cpt-cf-usage-collector-constraint-modkit`

**Touches**:
- Entities: `UsageRecord`

### Known Limitations / Technical Debt

- **Static `ModuleConfig`**: Has no dynamic update mechanism — changes require a gateway restart.
- **Outbox payload versioning**: Payload type `usage-collector.record.v1` uses an opaque string
  version with no schema registry or backward-compatibility contract defined in this feature.
  Payload versioning strategy MUST be documented before Feature 2+ extends the record schema.

## 6. Acceptance Criteria

- [ ] A usage source can run `factory.with_*().authorize(ctx, resource_id, resource_type)` and receive a `UsageEmitter` when the PDP permits `USAGE_RECORD`/`CREATE` for the given tenant and resource
- [ ] `enqueue()` persists a usage record to the source's local outbox using a pooled connection from the source's configured `DBRunner` provider; the resulting outbox row is durably committed when the pooled connection's implicit transaction commits
- [ ] `enqueue_in(db)` accepts a caller-supplied `&(dyn DBRunner + Sync)` (pooled connection or active transaction handle) and runs the outbox INSERT against it so the record is enqueued inside the caller's open transaction; rolling back the caller's transaction also rolls back the outbox row, and committing the caller's transaction is the durability boundary for the record
- [ ] Counter records without an idempotency key or with a negative value are rejected before the outbox INSERT; no outbox row is created
- [ ] Gauge records without a caller-supplied idempotency key are accepted; the emitter auto-generates a UUID v4 and the stored record always carries a non-null key
- [ ] Records with a metric name not in the module's allowed-metrics list are rejected by `enqueue()` in-memory before the outbox INSERT
- [ ] PDP denial in the factory's `.authorize()` step surfaces as `UsageEmitterError::PermissionDenied` (built via `UsageRecordError::permission_denied()` with reason `AUTHORIZATION_DENIED`) with no record persisted
- [ ] The outbox delivery pipeline delivers records to the gateway with at-least-once semantics; transient failures trigger exponential backoff retry with `outbox_backoff_max` configured below 15 minutes
- [ ] The gateway publishes the configurable metadata limit via `get_module_config`; the emitter enforces it. The gateway ingest endpoint resolves the active plugin via GTS and delegates record persistence with a 5 s default timeout
- [ ] The circuit breaker opens after 5 consecutive plugin call failures within a 10 s window; the gateway returns `503 Service Unavailable` while open; exactly one probe call is admitted after 30 s, with all other concurrent requests during the half-open window rejected until the probe completes
- [ ] `GET /usage-collector/v1/modules/{name}/config` returns the static allowed-metrics list for a configured module and 404 for an unknown module
- [ ] The no-op plugin accepts all write calls with no side effects and returns empty results for reads; integration tests pass with the no-op backend selected
- [ ] `dyn UsageEmitterRuntimeV1` is available in `ClientHub` after gateway `init()` completes; sources can call `runtime.factory(MODULE_NAME)` without additional setup
- [ ] Invalid configuration values (out-of-range `plugin_timeout`,
  `circuit_breaker.failure_threshold`, `circuit_breaker.recovery_timeout`,
  or `outbox_backoff_max`) are rejected at module/emitter initialization
  with a descriptive error; the process does not start.
- [ ] `factory.with_subject(s).authorize(...)` (explicit-with) binds `Subject { id, r#type }` into the returned `UsageEmitter` and emits PDP resource properties `SUBJECT_ID` (always) and `SUBJECT_TYPE` (only when `Subject.r#type` is `Some`) — the caller-supplied subject is never substituted by the gateway's own identity
- [ ] `factory.without_subject().authorize(...)` (explicit-without) leaves the resulting `UsageEmitter` with `subject = None` and omits both `SUBJECT_ID` and `SUBJECT_TYPE` from the PDP request entirely
- [ ] `factory.authorize(...)` with neither `.with_subject(...)` nor `.without_subject()` called (unset → `SubjectChoice::DefaultFromCtx`) derives the subject from `SecurityContext` at `.authorize()` time and emits PDP resource property `SUBJECT_ID` (the ctx-derived subject id), plus `SUBJECT_TYPE` when the ctx-derived subject carries a type
- [ ] `enqueue()` rejects a `UsageRecord` whose `subject: Option<Subject>` does not structurally equal the handle's bound subject (`record.subject != self.subject`) with `UsageEmitterError::PermissionDenied`; the serialized outbox record emits `subject` as a nested object when present and omits the `subject` key entirely when `None`

**Test data requirements**:
(1) Static gateway config must include at least one module with a `counter` metric and one with a
    `gauge` metric.
(2) PDP stub must support permit/deny configuration for `USAGE_RECORD`/`CREATE` actions by
    `tenant_id`/`resource_id` pair.
(3) Integration tests use `noop-usage-collector-plugin`.
(4) Idempotency collision test: submit two records with the same `idempotency_key` for the same
    metric and verify deduplication.

**Test coverage guidance**:
Unit: the factory's `.authorize()` step — PDP permit, PDP deny, handle expiry, metric type mismatch;
      `enqueue()` — each validation branch.
Integration: full emission flow with noop plugin; circuit breaker open/close cycle;
             dead-letter routing after max retries.
E2E: noop plugin store remains empty after transaction commit (noop backend discards all writes; read returns no records).
Performance baseline: measure `factory.with_*().authorize()` + `enqueue()` round-trip latency against
`nfr-ingestion-latency` target using noop backend.

**Success metrics**:
(1) At-least-once delivery rate ≥ 99.9% under normal conditions within `outbox_backoff_max` window.
(2) Circuit breaker recovers within 30 s of storage plugin recovery.
(3) Noop plugin integration test pass rate: 100% on CI.

## 7. Non-Applicability Notes

**COMPL (Regulatory & Privacy Compliance)**: Not applicable. This feature
processes opaque UUIDs (`tenant_id`, `resource_id`, `Subject.id`) and numeric
counters/gauges for internal billing metrics. No regulated personal data is
defined at the feature level. No audit trail, consent, data retention, or data
subject rights apply to this in-process SDK. If future features extend this to
personal data, COMPL must be revisited.

**UX (User Experience & Accessibility)**: Not applicable. This feature provides
an in-process SDK library (`usage-collector-sdk`, `usage-emitter`) and a gateway
service with machine-to-machine REST endpoints. There is no user-facing UI, no
end-user interaction model, no user-visible error messages, and no accessibility
requirements.
