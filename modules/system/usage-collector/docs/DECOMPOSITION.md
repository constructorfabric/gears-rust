# DECOMPOSITION — Usage Collector

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-status-overall`

<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Core SDK, Emitter & In-Process Ingest ⏳ HIGH](#21-core-sdk-emitter--in-process-ingest--high)
  - [2.2 REST Client & Remote Ingest Delivery ⏳ HIGH](#22-rest-client--remote-ingest-delivery--high)
  - [2.3 Usage Query API ⏳ HIGH](#23-usage-query-api--high)
  - [2.4 Production Storage Plugin ⏳ HIGH](#24-production-storage-plugin--high)
  - [2.5 Usage Type System ⏳ HIGH](#25-usage-type-system--high)
  - [2.6 Emission Rate Limiting ⏳ HIGH](#26-emission-rate-limiting--high)
  - [2.7 Retention Policy Management ⏳ MEDIUM](#27-retention-policy-management--medium)
  - [2.8 Operator Operations, Watermarks & Audit ⏳ MEDIUM](#28-operator-operations-watermarks--audit--medium)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

## 1. Overview

The Usage Collector DESIGN is decomposed into 8 features following a build-from-bottom-up strategy. The foundation layer (Feature 1) establishes the core SDK, emitter, in-process ingest pipeline, static metric configuration, and a no-op storage plugin that together enable end-to-end usage emission for in-process sources. The remote delivery layer (Feature 2) extends this to out-of-process sources via the REST client. Query capability (Feature 3) and the first production storage plugin (Feature 4) unlock real aggregation and raw query workflows. The type system (Feature 5) replaces static metric configuration with dynamic types-registry-backed validation. Rate limiting (Feature 6) adds per-source emission controls. Operator capabilities (Features 7–8) complete the system with retention management and the operator-initiated write operations (backfill, amendment, deactivation, watermarks, audit events).

**Decomposition Strategy**:

- Features grouped by functional cohesion (related capabilities together)
- Dependencies follow the SDK → emitter → gateway → plugin layering from the DESIGN
- Features 1–2 reflect the preliminary implementation already in progress; Features 3–8 are not yet started

**Deliberate Omissions**: None. All DESIGN components, sequences, data entities, FRs, principles, and constraints are covered.

## 2. Entries

### 2.1 [Core SDK, Emitter & In-Process Ingest](features/sdk-and-ingest-core/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-feature-sdk-and-ingest-core`
<!-- STATUS: IN_PROGRESS — all p1 DoD items implemented; one p1 instruction gap (inst-authz-1: no-open-transaction assertion deferred — requires modkit-db framework API change). -->

- **Type**: Service

- **Purpose**: Establish the core data model, delivery traits, two-phase authorization emitter, transactional outbox pipeline, gateway ingest handler, static metric configuration, and no-op storage plugin. Delivers the complete in-process emission path from `authorize_for()` through the outbox background pipeline to the gateway and plugin. This is the prerequisite for all other features.

- **Depends On**: None

- **Scope**:
  - `usage-collector-sdk` crate: client and plugin-client trait definitions, shared usage record model types, GTS schema for plugin registration, error types
  - `usage-emitter` crate: scoped emitter with two-phase authorization flow (pre-authorization and per-record authorization), transactional outbox enqueue, background outbox delivery handler
  - `usage-collector` gateway crate: plugin resolution via GTS with timeout enforcement, outbox queue registration and schema migrations, record ingest endpoint, module config fetch endpoint, static metric configuration
  - `noop-usage-collector-storage-plugin` crate: no-op storage plugin implementation for tests and local development
  - Owns authorization check on all inbound requests via `component-gateway`
  <!-- Metrics Config Boundary: F1 owns collection config (what data to collect, sampling rates, collection intervals); F5 owns reporting/export config (exporters, dashboards, alerting thresholds) -->

- **Out of scope**:
  - Remote HTTP delivery (Feature 2)
  - Query API (Feature 3)
  - Real storage backend plugins (Feature 4)
  - Types-registry type validation (Feature 5)
  - Rate limiting enforcement (Feature 6)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-fr-ingestion`
  - [ ] `p1` - `cpt-cf-usage-collector-fr-idempotency`
  - [ ] `p1` - `cpt-cf-usage-collector-fr-delivery-guarantee`
  - [ ] `p1` - `cpt-cf-usage-collector-fr-counter-semantics`
  - [ ] `p1` - `cpt-cf-usage-collector-fr-gauge-semantics`
  - [ ] `p1` - `cpt-cf-usage-collector-fr-tenant-attribution`
  - [ ] `p1` - `cpt-cf-usage-collector-fr-resource-attribution`
  - [ ] `p1` - `cpt-cf-usage-collector-fr-subject-attribution`
  - [ ] `p1` - `cpt-cf-usage-collector-fr-tenant-isolation`
  - [ ] `p1` - `cpt-cf-usage-collector-fr-ingestion-authorization`
  - [ ] `p1` - `cpt-cf-usage-collector-fr-pluggable-storage`
  - [ ] `p2` - `cpt-cf-usage-collector-fr-record-metadata`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-availability`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-ingestion-latency`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-authentication`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-authorization`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-scalability`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-fault-tolerance`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-recovery` (owner: F1)
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-graceful-degradation`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-rpo` (owner: F1)

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-principle-source-side-persistence`
  - [ ] `p1` - `cpt-cf-usage-collector-principle-pluggable-storage`
  - [ ] `p1` - `cpt-cf-usage-collector-principle-tenant-from-ctx`
  - [ ] `p1` - `cpt-cf-usage-collector-principle-fail-closed`
  - [ ] `p1` - `cpt-cf-usage-collector-principle-scoped-source-attribution`
  - [ ] `p1` - `cpt-cf-usage-collector-principle-two-phase-authz`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-constraint-outbox-infra`
  - [ ] `p1` - `cpt-cf-usage-collector-constraint-single-plugin`
  - [ ] `p1` - `cpt-cf-usage-collector-constraint-modkit`
  - [ ] `p1` - `cpt-cf-usage-collector-constraint-security-context`
  - [ ] `p1` - `cpt-cf-usage-collector-constraint-no-business-logic`

- **Domain Model Entities**:
  - `UsageRecord` — DEFINES: UsageRecord schema (canonical data model for usage events)
  - `AuthorizedUsageEmitter`
  - `ModuleConfig` / `AllowedMetric`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-usage-collector-component-sdk`
  - [ ] `p1` - `cpt-cf-usage-collector-component-emitter`
    > **Owner**: F1 — initialises and configures the emitter
    > **Used by**: F5 (type validation in authorize_for), F6 (SDK-side quota check in authorize_for)
  - [ ] `p1` - `cpt-cf-usage-collector-component-gateway`
    > **Owner**: F1 — initialises and configures the gateway; owns authorization check on all inbound requests
    > **Used by**: F2 (passes pre-authenticated requests through), F3 (query handlers), F5 (defense-in-depth validation + types endpoint), F6 (gateway-side rate limiting), F7 (retention policy CRUD), F8 (backfill, amendment, deactivation, watermarks, audit)
  - [ ] `p1` - `cpt-cf-usage-collector-component-storage-plugin` (noop only)
    > **Owner**: F4 — implements the full production storage plugin trait
    > **Used by**: F1 (noop implementation only), F3 (query operations), F7 (enforce_retention), F8 (backfill_ingest, amend_record, deactivate_record, get_watermarks)

- **API**:
  - POST /usage-collector/v1/records
  - GET /usage-collector/v1/modules/{module_name}/config

- **Sequences**:

  - `cpt-cf-usage-collector-seq-emit`  <!-- Shared Sequence Participation: F1 role = initiator (triggers emission — authorizes and enqueues the record into the outbox) -->

- **Data**:
  - `usage-records` table (written by storage plugin on ingest)
  - [ ] `cpt-cf-usage-collector-dbtable-outbox`

- **Phases/Milestones**:
  - Phase 1: SDK crate (`usage-collector-sdk`) — core traits, data model, error types
  - Phase 2: Emitter crate (`usage-emitter`) — two-phase authorization, outbox enqueue, background delivery
  - Phase 3: Gateway crate (`usage-collector`) — plugin resolution, ingest endpoint, config endpoint, static metric config
  - Phase 4: No-op storage plugin (`noop-usage-collector-storage-plugin`) — test/local-dev plugin

---

### 2.2 [REST Client & Remote Ingest Delivery](features/rest-ingest/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-feature-rest-ingest`

- **Type**: Service

- **Purpose**: Enable out-of-process usage sources to emit records using the same `UsageEmitterV1` API as in-process sources. The `usage-collector-rest-client` crate registers `UsageEmitterV1` in `ClientHub` backed by an HTTP client that acquires a bearer token from the AuthN resolver and POSTs records to the gateway ingest endpoint with identical authorization and validation semantics.

- **Depends On**: `cpt-cf-usage-collector-feature-sdk-and-ingest-core`

- **Scope**:
  - `usage-collector-rest-client` crate: `UsageCollectorRestClient` implementing `UsageCollectorClientV1`, bearer token acquisition via authn-resolver (client credentials flow), HTTP POST to `POST /usage-collector/v1/records`, outbox schema migrations registration, `HandlerResult` mapping (Retry on 5xx/429/network errors; Reject on permanent 4xx)
  - Gateway two-level authorization model for REST ingestion: service-to-service bearer token authentication of the forwarder plus gateway-level PDP check against the forwarder's service identity

- **Out of scope**:
  - In-process emission pipeline (Feature 1)
  - Query endpoints (Feature 3)
  - Re-evaluation of the original caller's authorization context from F1 — the gateway performs service-to-service bearer token authentication of the forwarder and a gateway-level PDP check against the forwarder's service identity, but does NOT duplicate or re-check the authorization decisions already made upstream for the original caller

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-fr-rest-ingestion`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-principle-fail-closed`
  - [ ] `p1` - `cpt-cf-usage-collector-principle-tenant-from-ctx`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-constraint-security-context`

- **Domain Model Entities**:
  - `UsageRecord` — USES: UsageRecord (defined in F1) — reads records for delivery

- **Design Components**:

  - [ ] `p1` - `cpt-cf-usage-collector-component-rest-client`

- **API**:
  - POST /usage-collector/v1/records (client-side outbox delivery)
  - GET /usage-collector/v1/modules/{module_name}/config (client-side config fetch)

- **Sequences**:

  - `cpt-cf-usage-collector-seq-emit-remote`

- **Data**:
  - None (stateless delivery hop; records persisted in source's local outbox)

- **Phases/Milestones**: none

---

### 2.3 [Usage Query API](features/query-api/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-feature-query-api`

- **Type**: REST API Layer

- **Purpose**: Enable authorized consumers to retrieve aggregated and raw usage data from the gateway so tenants and operators can inspect, audit, and act on collected usage records.

- **Depends On**: `cpt-cf-usage-collector-feature-sdk-and-ingest-core`

- **Scope**:
  - `usage-collector-sdk` additions: `AggregationQuery`, `AggregationResult`, `RawQuery`, `PagedResult` types; `query_aggregated` and `query_raw` operations on `UsageCollectorPluginClientV1`
  - `usage-collector` gateway: `GET /usage-collector/v1/aggregated` handler (tenant from SecurityContext, PDP authorization + constraint application, delegation to plugin), `GET /usage-collector/v1/raw` handler (same authorization pattern, cursor-based pagination)
  - Plugin trait query operations: `query_aggregated` (aggregation functions SUM/COUNT/MIN/MAX/AVG, optional filters, GROUP BY dimensions pushed down to storage engine), `query_raw` (tenant-scoped, optional filters, cursor pagination)

- **Out of scope**:
  - Ingest pipeline (Feature 1)
  - Production storage backend implementation (Feature 4)
  - Watermark metadata query (Feature 8)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-fr-query-aggregation`
  - [ ] `p2` - `cpt-cf-usage-collector-fr-query-raw`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-workload-isolation`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-principle-fail-closed`
  - [ ] `p1` - `cpt-cf-usage-collector-principle-tenant-from-ctx`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-constraint-security-context`
  - [ ] `p1` - `cpt-cf-usage-collector-constraint-no-business-logic`

- **Domain Model Entities**:
  - `AggregationQuery`
  - `AggregationResult`
  - `RawQuery`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-usage-collector-component-gateway` (query handlers)
  - [ ] `p1` - `cpt-cf-usage-collector-component-storage-plugin` (query operations)

- **API**:
  - GET /usage-collector/v1/aggregated
  - GET /usage-collector/v1/raw

- **Sequences**:

  - `cpt-cf-usage-collector-seq-query-aggregated`
  - `cpt-cf-usage-collector-seq-query-raw`

- **Data**:
  - `usage-records` table (read by storage plugin for aggregation and raw queries)

- **Phases/Milestones**: none

---

### 2.4 [Production Storage Plugin](features/production-storage-plugin/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-feature-production-storage-plugin`

- **Type**: Plugin Interface

- **Purpose**: Provide durable, high-throughput storage for usage records so the system can meet its throughput, latency, and recovery objectives with a real storage backend instead of the no-op placeholder.

- **Depends On**: `cpt-cf-usage-collector-feature-sdk-and-ingest-core`, `cpt-cf-usage-collector-feature-query-api`

- **Scope**:
  - First production plugin crate implementing the full storage plugin trait
  - Idempotent record ingest keyed on idempotency key; counter delta accumulation semantics
  - Aggregation query pushdown to the storage engine with optional pre-aggregated acceleration; cursor-based raw query pagination with tenant and dimension filtering
  - Operator write operations: backfill ingest, record amendment, record deactivation
  - Retention enforcement and watermark retrieval operations
  - GTS schema registration, database schema migrations, encrypted connections to the storage backend

- **Out of scope**:
  - No-op plugin (Feature 1)
  - Second storage backend

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-fr-pluggable-storage`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-query-latency`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-throughput`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-rpo` (applies-to: all — F4 must independently satisfy the RPO constraint via durable storage backend)
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-recovery` (applies-to: all — F4 must independently satisfy recovery via storage backend durability guarantees)
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-retention`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-principle-pluggable-storage`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-constraint-single-plugin`
  - [ ] `p1` - `cpt-cf-usage-collector-constraint-types-registry` (GTS schema for plugin registration)
  - [ ] `p1` - `cpt-cf-usage-collector-constraint-encryption`

- **Domain Model Entities**:
  - `UsageRecord` — USES: UsageRecord (defined in F1) — reads and persists records for storage
  - `RetentionPolicy` (enforced by plugin)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-usage-collector-component-storage-plugin` (production implementation)

- **API**:
  - None (internal plugin interface only)

- **Sequences**:

  - `cpt-cf-usage-collector-seq-emit` (storage write path)  <!-- Shared Sequence Participation: F4 role = buffer (persists the record durably via the production storage plugin) -->
  - `cpt-cf-usage-collector-seq-query-aggregated` (storage read path)
  - `cpt-cf-usage-collector-seq-query-raw` (storage read path)

- **Data**:
  - `usage-records` table (primary storage: idempotent upsert on ingest, read for aggregation and raw queries)
  - [ ] `cpt-cf-usage-collector-dbtable-records`
  - [ ] `cpt-cf-usage-collector-dbtable-counter-accumulation`

- **Phases/Milestones**:
  - Phase 1: Storage backend selection (ClickHouse or TimescaleDB) and schema design
  - Phase 2: Ingest operations — idempotent upsert, counter delta accumulation, GTS schema registration, migrations
  - Phase 3: Query operations — aggregation pushdown, cursor-based raw pagination
  - Phase 4: Operator write operations — backfill ingest, record amendment, deactivation, watermarks, retention enforcement

---

### 2.5 [Usage Type System](features/type-system/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-feature-type-system`

- **Type**: Service

- **Purpose**: Ensure every emitted record conforms to a registered usage type schema so invalid or unknown metric kinds are rejected before reaching the storage backend and operators can register custom measuring units.

- **Depends On**: `cpt-cf-usage-collector-feature-sdk-and-ingest-core`

- **Scope**:
  - `usage-emitter`: fetch registered usage type schema from types-registry during `authorize_for()` phase 1; validate the record against the schema in-memory before the outbox INSERT; surface validation failures as `UsageEmitterError` before any domain operation is committed
  - `usage-collector` gateway: defense-in-depth schema validation on ingest before delegating to plugin; `POST /usage-collector/v1/types` endpoint delegating entirely to types-registry for custom usage type registration (name, metric kind, unit label)
  - `UsageType` domain entity (owned by types-registry; fetched by emitter and gateway)

- **Out of scope**:
  - Static `metrics` config from Feature 1 (retained as the allowed-metrics list; type schemas are now sourced from types-registry)
  - Storage plugin implementation (Feature 4)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-fr-type-validation`
  - [ ] `p1` - `cpt-cf-usage-collector-fr-custom-units`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-principle-fail-closed`
  - [ ] `p1` - `cpt-cf-usage-collector-principle-two-phase-authz`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-constraint-types-registry`

- **Domain Model Entities**:
  - `UsageType`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-usage-collector-component-emitter` (type validation in authorize_for)
  - [ ] `p1` - `cpt-cf-usage-collector-component-gateway` (defense-in-depth validation + types endpoint)

- **API**:
  - POST /usage-collector/v1/types

- **Sequences**:

  - `cpt-cf-usage-collector-seq-emit` (type validation in phase 1)  <!-- Shared Sequence Participation: F5 role = monitor (validates record schema before outbox INSERT; observes emission health via defense-in-depth gateway check) -->
  <!-- Metrics Config Boundary: F5 owns reporting/export config (exporters, dashboards, alerting thresholds); F1 owns collection config (what data to collect, sampling rates, collection intervals) -->

- **Data**:
  - None (type schemas managed in types-registry, not locally)

- **Phases/Milestones**:
  - Phase 1: Emitter integration — types-registry fetch and in-memory schema validation in `authorize_for()` phase 1
  - Phase 2: Gateway integration — defense-in-depth validation on ingest and `POST /usage-collector/v1/types` delegation endpoint

---

### 2.6 [Emission Rate Limiting](features/rate-limiting/) ⏳ HIGH

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-feature-rate-limiting`

- **Type**: Service

- **Purpose**: Prevent outbox flooding by enforcing per-source emission quotas at SDK `authorize_for()` phase 1 before any transaction opens. Supplement with per-(source, tenant) rate limit enforcement at the gateway on ingest to protect the storage backend from quota-exhausting sources.

- **Depends On**: `cpt-cf-usage-collector-feature-sdk-and-ingest-core`

- **Scope**:
  - `usage-emitter` phase 1 extension: `authorize_for()` fetches the current source-level emission quota and window snapshot from the gateway; `enqueue()` evaluates the quota in-memory and rejects before the outbox INSERT if the source quota is exhausted
  - `usage-collector` gateway ingest extension: per-(source, tenant) rate limit check on `POST /usage-collector/v1/records`
  - Rate limit configuration: configurable window and quota per source module set via static deployment configuration (no public REST configuration endpoint in this feature)
  - Observability: rejected emissions surfaced via pluggable metrics interface

- **Out of scope**:
  - Backfill rate limiting (separate independent rate limits, Feature 8)
  - Type validation (Feature 5)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-fr-rate-limiting`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-principle-two-phase-authz`
  - [ ] `p1` - `cpt-cf-usage-collector-principle-fail-closed`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-constraint-security-context`

- **Domain Model Entities**:
  - None (quota window state managed by the gateway, not a named domain entity)

- **Design Components**:

  - [ ] `p1` - `cpt-cf-usage-collector-component-emitter` (SDK-side quota check in authorize_for)
  - [ ] `p1` - `cpt-cf-usage-collector-component-gateway` (gateway-side per-(source, tenant) rate limiting)

- **API**:
  - None (internal enforcement; no public configuration endpoint in this feature)

- **Sequences**:

  - `cpt-cf-usage-collector-seq-emit` (quota check in phase 1)  <!-- Shared Sequence Participation: F6 role = rate-gate (throttles emission rate — rejects at authorize_for phase 1 when source quota is exhausted before outbox INSERT) -->
  <!-- Rate Limit vs Quota Boundary: F6 enforces short-window per-(source, tenant) rate limits on real-time ingest; F8 enforces long-window per-tenant quota limits via backfill and operator-controlled caps -->

- **Data**:
  - None (quota tracking is in-memory or in the gateway's local state)

- **Phases/Milestones**:
  - Phase 1: Emitter-side quota check — `authorize_for()` extension with quota fetch and in-memory evaluation
  - Phase 2: Gateway-side rate limiting — per-(source, tenant) check on ingest endpoint and observability integration

---

### 2.7 [Retention Policy Management](features/retention-policies/) ⏳ MEDIUM

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-feature-retention-policies`

- **Type**: Background Task

- **Purpose**: Allow platform operators to configure data retention policies at global, per-tenant, and per-usage-type scopes. Enforce these policies via the storage plugin using storage-native TTL or scheduled deletion, with a mandatory global default that cannot be deleted.

- **Depends On**: `cpt-cf-usage-collector-feature-sdk-and-ingest-core`, `cpt-cf-usage-collector-feature-production-storage-plugin`

- **Scope**:
  - `RetentionPolicy` domain entity: scope (`global` / `tenant` / `usage-type`), target identifier, retention duration
  - Gateway REST API: `GET /usage-collector/v1/retention` (list), `PUT /usage-collector/v1/retention/{scope}` (create/update), `DELETE /usage-collector/v1/retention/{scope}` (non-global only; global default deletion rejected)
  - Precedence rule enforcement: per-usage-type > per-tenant > global
  - Plugin `enforce_retention` operation: permanent hard delete of expired records via storage-native TTL or scheduled deletion; triggered by the gateway on a background schedule
  - `inactive` status is reserved for operator amendment (Feature 8) and MUST NOT be used by retention enforcement

- **Out of scope**:
  - Amendment and deactivation (Feature 8)
  - Rate limiting (Feature 6)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-fr-retention-policies`
  - [ ] `p1` - `cpt-cf-usage-collector-nfr-retention`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-principle-fail-closed`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-constraint-security-context`
  - [ ] `p1` - `cpt-cf-usage-collector-constraint-no-business-logic`

- **Domain Model Entities**:
  - `RetentionPolicy`

- **Design Components**:

  - [ ] `p1` - `cpt-cf-usage-collector-component-gateway` (retention policy CRUD)
  - [ ] `p1` - `cpt-cf-usage-collector-component-storage-plugin` (enforce_retention)

- **API**:
  - GET /usage-collector/v1/retention
  - PUT /usage-collector/v1/retention/{scope}
  - DELETE /usage-collector/v1/retention/{scope}

- **Sequences**:
  - None (policy management is synchronous CRUD; enforcement runs as a background task)

- **Data**:
  - `retention-policies` table (retention policy CRUD and enforcement state)

- **Phases/Milestones**:
  - Phase 1: `RetentionPolicy` domain entity, gateway REST API (GET/PUT/DELETE), precedence rule enforcement
  - Phase 2: Plugin `enforce_retention` operation — background schedule integration and storage-native TTL/deletion

---

### 2.8 [Operator Operations, Watermarks & Audit](features/operator-ops/) ⏳ MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-feature-operator-ops`

- **Type**: Service

- **Purpose**: Complete the operator-facing capability set: backfill for historical record ingestion with a gateway-local outbox pipeline and independent rate limits, individual record amendment and deactivation, watermark metadata exposure for external reconciliation, and structured `WriteAuditEvent` emission to `audit_service` for every operator-initiated write.

- **Depends On**: `cpt-cf-usage-collector-feature-sdk-and-ingest-core`, `cpt-cf-usage-collector-feature-query-api`, `cpt-cf-usage-collector-feature-production-storage-plugin`

- **Scope**:
  - Backfill: `POST /usage-collector/v1/backfill`; PDP authorization for specified `tenant_id`; configurable time boundary enforcement (max window default 90 days, future tolerance default 5 min, elevated authz beyond max window); gateway-local backfill outbox (`Outbox::enqueue_batch` within handler transaction); internal `MessageHandler` calling `plugin.backfill_ingest()`; independent rate limits and lower processing priority relative to real-time ingest; `202 Accepted` response
  - Amendment: `PATCH /usage-collector/v1/records/{id}`; PDP authorization; `plugin.amend_record()` with optimistic concurrency
  - Deactivation: `POST /usage-collector/v1/records/{id}/deactivate`; PDP authorization; `plugin.deactivate_record()` (sets `status = inactive`, record retained for audit)
  - Watermarks: `GET /usage-collector/v1/metadata/watermarks`; calls `plugin.get_watermarks()` returning per-source and per-tenant event counts and latest ingested timestamps
  - Audit: `WriteAuditEvent` emitted to platform `audit_service` after each completed operator write (backfill on outbox commit, amendment, deactivation); best-effort with ≤2s timeout; emission failures logged and monitored but do not roll back or fail the primary operation

- **Out of scope**:
  - Retention policy management (Feature 7)
  - Rate limiting for real-time ingest (Feature 6)
  <!-- Rate Limit vs Quota Boundary: F8 enforces long-window per-tenant quota limits (backfill rate limits, operator-controlled write caps); F6 enforces short-window per-(source, tenant) rate limits on real-time ingest -->

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-usage-collector-fr-backfill-api`
  - [ ] `p2` - `cpt-cf-usage-collector-fr-event-amendment`
  - [ ] `p3` - `cpt-cf-usage-collector-fr-backfill-boundaries`
  - [ ] `p3` - `cpt-cf-usage-collector-fr-metadata-exposure`
  - [ ] `p1` - `cpt-cf-usage-collector-fr-audit`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-principle-fail-closed`
  - [ ] `p1` - `cpt-cf-usage-collector-principle-tenant-from-ctx`

- **Design Constraints Covered**:

  - [ ] `p1` - `cpt-cf-usage-collector-constraint-security-context`
  - [ ] `p1` - `cpt-cf-usage-collector-constraint-no-business-logic`

- **Domain Model Entities**:
  - `BackfillOperation`
  - `WriteAuditEvent`

- **Design Components**:

  - [ ] `p2` - `cpt-cf-usage-collector-component-gateway` (backfill, amendment, deactivation, watermarks, audit)
  - [ ] `p2` - `cpt-cf-usage-collector-component-storage-plugin` (backfill_ingest, amend_record, deactivate_record, get_watermarks)

- **API**:
  - POST /usage-collector/v1/backfill
  - PATCH /usage-collector/v1/records/{id}
  - POST /usage-collector/v1/records/{id}/deactivate
  - GET /usage-collector/v1/metadata/watermarks

- **Sequences**:

  - `cpt-cf-usage-collector-seq-backfill`
  - `cpt-cf-usage-collector-seq-amend`
  - `cpt-cf-usage-collector-seq-deactivate`

- **Data**:
  - `usage-records` table (amendment and deactivation write path; watermarks read)

- **Phases/Milestones**:
  - Phase 1: Backfill endpoint (`POST /usage-collector/v1/backfill`) — gateway-local outbox, `backfill_ingest`, configurable time boundaries, independent rate limits
  - Phase 2: Amendment (`PATCH /usage-collector/v1/records/{id}`) and deactivation (`POST /usage-collector/v1/records/{id}/deactivate`)
  - Phase 3: Watermarks endpoint (`GET /usage-collector/v1/metadata/watermarks`) and structured `WriteAuditEvent` emission

---

## 3. Feature Dependencies

```text
cpt-cf-usage-collector-feature-sdk-and-ingest-core  (foundation)
    ├─→ cpt-cf-usage-collector-feature-rest-ingest
    ├─→ cpt-cf-usage-collector-feature-type-system
    ├─→ cpt-cf-usage-collector-feature-rate-limiting
    ├─→ cpt-cf-usage-collector-feature-query-api
    │       └─→ cpt-cf-usage-collector-feature-production-storage-plugin
    │               ├─→ cpt-cf-usage-collector-feature-retention-policies  (also ←F1)
    │               └─→ cpt-cf-usage-collector-feature-operator-ops         (also ←F1, ←F3)
    └─→ (see above: F7 depends on F1+F4; F8 depends on F1+F3+F4)
```

**Dependency Rationale**:

- `cpt-cf-usage-collector-feature-rest-ingest` requires `cpt-cf-usage-collector-feature-sdk-and-ingest-core`: the REST client implements `UsageCollectorClientV1` defined in the SDK and delivers records to the gateway ingest endpoint introduced in Feature 1.
- `cpt-cf-usage-collector-feature-query-api` requires `cpt-cf-usage-collector-feature-sdk-and-ingest-core`: query operations extend the `UsageCollectorPluginClientV1` trait and add gateway handlers that follow the same PDP + plugin delegation pattern established in Feature 1.
- `cpt-cf-usage-collector-feature-production-storage-plugin` requires `cpt-cf-usage-collector-feature-sdk-and-ingest-core` and `cpt-cf-usage-collector-feature-query-api`: a production plugin must implement all plugin trait operations including `query_aggregated` and `query_raw` introduced in Feature 3.
- `cpt-cf-usage-collector-feature-type-system` requires `cpt-cf-usage-collector-feature-sdk-and-ingest-core`: type validation integrates into the `authorize_for()` phase 1 flow and gateway ingest handler established in Feature 1; independent of query and plugin features.
- `cpt-cf-usage-collector-feature-rate-limiting` requires `cpt-cf-usage-collector-feature-sdk-and-ingest-core`: quota checks extend the `authorize_for()` phase 1 flow and gateway ingest handler from Feature 1; independent of query and plugin features.
- `cpt-cf-usage-collector-feature-retention-policies` requires `cpt-cf-usage-collector-feature-sdk-and-ingest-core` and `cpt-cf-usage-collector-feature-production-storage-plugin`: retention enforcement calls `plugin.enforce_retention()` which requires a real storage backend.
- `cpt-cf-usage-collector-feature-operator-ops` requires `cpt-cf-usage-collector-feature-sdk-and-ingest-core`, `cpt-cf-usage-collector-feature-query-api`, and `cpt-cf-usage-collector-feature-production-storage-plugin`: backfill introduces a gateway-local outbox queue; amendment and deactivation extend the plugin write trait; watermarks extend the plugin query interface; all require a real storage backend.
- `cpt-cf-usage-collector-feature-rest-ingest`, `cpt-cf-usage-collector-feature-type-system`, and `cpt-cf-usage-collector-feature-rate-limiting` are independent of each other and can be developed in parallel after Feature 1.
- `cpt-cf-usage-collector-feature-retention-policies` and `cpt-cf-usage-collector-feature-operator-ops` are independent of each other and can be developed in parallel after Feature 4.
