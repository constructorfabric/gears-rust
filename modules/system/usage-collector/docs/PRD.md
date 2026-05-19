# PRD — Usage Collector


<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
  - [2.3 Actor Permissions](#23-actor-permissions)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Usage Ingestion](#51-usage-ingestion)
  - [5.2 Metric Semantics](#52-metric-semantics)
  - [5.3 Delivery Guarantee](#53-delivery-guarantee)
  - [5.4 Attribution & Isolation](#54-attribution--isolation)
  - [5.5 Pluggable Storage](#55-pluggable-storage)
  - [5.6 Usage Query & Aggregation](#56-usage-query--aggregation)
  - [5.7 Retention Policy Management](#57-retention-policy-management)
  - [5.8 Backfill & Amendment](#58-backfill--amendment)
  - [5.9 Usage Type System](#59-usage-type-system)
  - [5.10 Audit Events](#510-audit-events)
  - [5.11 Rate Limiting](#511-rate-limiting)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Module-Specific NFRs](#61-module-specific-nfrs)
  - [6.2 Data Governance](#62-data-governance)
  - [6.3 NFR Exclusions](#63-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

A usage metering module for collecting usage records from platform services and providing aggregated usage data to clients. The Usage Collector is the centralized store and query engine for all platform usage data. It receives usage records from sources, persists them durably in a pluggable storage backend, and serves aggregated results to downstream consumers.

### 1.2 Background / Problem Statement

Platform services need a centralized place to report resource consumption (API calls, AI tokens, storage bytes, compute hours) so that downstream systems (billing, quota enforcement, dashboards) can operate on consistent data. Without a central usage module, each consumer implements its own collection logic, leading to inconsistent data, duplicated effort, and no single source of truth.

The Usage Collector addresses this by accepting usage records from sources and providing a query/aggregation API to consumers. Business logic (pricing, billing rules, invoice generation, quota enforcement decisions) remains the responsibility of downstream consumers.

### 1.3 Goals (Business Outcomes)

- **Centralized metering**: All platform services that measure resource consumption report to a single authoritative store, eliminating per-service tracking implementations and data inconsistencies across the platform.
- **Zero record loss**: Every usage record successfully emitted by a source is eventually persisted and queryable — no record is silently lost due to source restarts, process failures, or transient storage unavailability.
- **Operator self-service for new resource types**: Platform operators can onboard new billable resource types (e.g., GPU hours, custom credit units) via API without code changes or service redeployment, supporting rapid product iteration.
- **Downstream consumers need no aggregation layer**: Billing, quota enforcement, and dashboard systems obtain aggregated usage views directly from the Usage Collector within interactive latency bounds, without maintaining their own aggregation infrastructure.

**Success Metrics** (measured at initial production release):

| Goal | Measurable Success Criterion | Target |
|------|------------------------------|--------|
| Centralized metering | All existing platform services with billable operations integrated with Usage Collector SDK; zero per-service custom metering implementations remaining | 100% of billable services at first production deployment |
| Zero record loss | Zero records silently discarded in soak testing over a 7-day period including storage backend failure injection; all failures surfaced via `outbox_dead_rows_total` monitoring | 0 silent losses over 7-day soak test |
| Operator self-service | Time to onboard a new billable resource type (from API call to first emittable records) without code changes or redeployment | ≤ 5 minutes end-to-end |
| Downstream consumers need no aggregation layer | All registered downstream consumers (billing, quota, dashboards) serve their primary aggregation use cases via the Usage Collector query API; no downstream-maintained aggregation tables required at launch | 0 downstream aggregation tables at first production deployment |

### 1.4 Glossary

| Term                   | Definition                                                                                                                                                                                                                                                         |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Usage Record           | A single data point representing resource consumption by a tenant, with a numeric value and a timestamp                                                                                                                                                            |
| Counter                | A delta metric representing a non-negative increment since the last report (e.g., API calls in this batch). The Usage Collector accumulates submitted deltas into a monotonically increasing persistent total.                                                     |
| Gauge                  | A point-in-time metric that can go up or down (e.g., current memory usage in bytes). Stored as-is without monotonicity constraints.                                                                                                                                |
| Idempotency Key        | A client-provided identifier ensuring at-least-once processing without duplicate records                                                                                                                                                                           |
| Usage Collector Plugin | A storage backend plugin for a specific database (ClickHouse, TimescaleDB)                                                                                                                                                                                         |
| Record Metadata        | An optional, extensible JSON object attached to a usage record, allowing usage sources to include context-specific properties (e.g., LLM model name, token type, geographic region) that are opaque to the Usage Collector and interpreted by downstream consumers |
| Measuring Unit         | A registered schema defining how a specific usage type is measured (e.g., "ai-credits", "vCPU-hours", "gpu-hours")                                                                                                                                                 |
| Backfill               | The process of bulk-inserting historical usage records to fill gaps caused by outages, pipeline failures, or corrections. Backfill is purely additive and does not modify existing records.                                                                        |
| Amendment              | A correction to previously recorded usage data, either by updating properties of an individual event or deprecating it                                                                                                                                             |
| Reconciliation         | The process of comparing usage data across pipeline stages or external sources to detect gaps and inconsistencies (performed by external systems; the Usage Collector exposes metadata to support this)                                                            |
| Rate Limit Window      | A configurable time interval over which a source's emission count is tracked for rate limiting purposes (e.g., per second, per minute)                                                                                                                             |
| Emission Quota         | The maximum number of usage records a source module is permitted to emit across all tenants within a single rate limit window, enforced at the source by the SDK. Per-tenant granularity is enforced separately at the gateway on ingest.                         |
| PDP                    | Policy Decision Point — the platform authorization service (`authz-resolver`) that evaluates access control policies and returns permit/deny decisions, optionally with query constraint filters that narrow the scope of permitted operations                     |
| SecurityContext        | A platform-provided, server-side structure that carries the authenticated caller's identity, including tenant ID and subject (user or service account) identity. Derived from the request authentication token; never accepted from request payloads               |

## 2. Actors

### 2.1 Human Actors

#### Platform Operator

**ID**: `cpt-cf-usage-collector-actor-platform-operator`

- **Role**: Deploys and configures the usage collector module, selects storage backend, monitors system health.
- **Needs**: Ability to choose and configure storage backends without code changes.

#### Platform Developer

**ID**: `cpt-cf-usage-collector-actor-platform-developer`

- **Role**: Integrates platform services with the Usage Collector using the SDK or API to emit usage data.
- **Needs**: Well-documented SDK for emitting usage data with minimal integration effort.

#### Tenant Administrator

**ID**: `cpt-cf-usage-collector-actor-tenant-admin`

- **Role**: Queries raw and aggregated usage data for their tenant.
- **Needs**: Access to raw and aggregated usage records filtered by type, subject, and resource for their tenant only, with time-range filtering.

### 2.2 System Actors

#### Usage Source

**ID**: `cpt-cf-usage-collector-actor-usage-source`

- **Role**: Any system that produces usage records. This includes in-process platform services using the SDK (e.g., LLM Gateway, Compute Service, API Gateway), remote CyberFabric nodes forwarding collected records via the REST API, and external non-CyberFabric systems submitting records directly via the REST API using OAuth2 client credentials.

#### Usage Consumer

**ID**: `cpt-cf-usage-collector-actor-usage-consumer`

- **Role**: Any system that queries aggregated usage data (e.g., billing system, quota enforcer, dashboard).

#### Storage Backend

**ID**: `cpt-cf-usage-collector-actor-storage-backend`

- **Role**: The underlying data store (ClickHouse or TimescaleDB) that persists usage records.

#### Types Registry

**ID**: `cpt-cf-usage-collector-actor-types-registry`

- **Role**: Provides schema validation for usage types and custom measuring units.

#### Monitoring System

**ID**: `cpt-cf-usage-collector-actor-monitoring-system`

- **Role**: Consumes usage metadata for dashboards, alerting, and operational visibility.

### 2.3 Actor Permissions

| Actor | Permitted Operations | Denied by Default |
|-------|---------------------|-------------------|
| `cpt-cf-usage-collector-actor-platform-operator` | Configure and delete retention policies (except global default); submit backfill operations; amend and deactivate individual records; register custom usage types; view watermarks and ingestion metadata | Querying or modifying records belonging to any tenant without an explicit security context; deleting the global default retention policy |
| `cpt-cf-usage-collector-actor-platform-developer` | Emit usage records via the SDK within the source module's authorized metric namespace and tenant scope | Emitting metrics outside the source module's authorized namespace; attributing records to subjects or resources outside the authorized scope |
| `cpt-cf-usage-collector-actor-tenant-admin` | Query aggregated and raw usage records scoped to their own tenant; view watermarks for their tenant | Accessing usage data of any other tenant; invoking operator-only operations (backfill, amendment, deactivation, retention policy management) |
| `cpt-cf-usage-collector-actor-usage-source` | Emit metrics valid for the claimed module type via the SDK or REST API; the scope of permitted target tenants is enforced by the platform PDP at emit time — the caller must be PDP-authorized for the tenant supplied in the record, covering both same-tenant and parent→subtenant scenarios | Emitting records attributed to tenants outside the PDP-authorized scope; emitting metrics not registered for the claimed module type |
| `cpt-cf-usage-collector-actor-usage-consumer` | Query aggregated and raw usage data scoped to the authenticated tenant; subject to PDP constraint filters | Accessing cross-tenant data; mutating usage records |
| `cpt-cf-usage-collector-actor-storage-backend` | Receive and persist usage records forwarded by the gateway plugin; respond to query and retention operations initiated by the plugin | Direct access from any actor other than the authorized storage plugin instance |
| `cpt-cf-usage-collector-actor-types-registry` | Respond to schema registration and validation requests initiated by the gateway | N/A — passive service; does not initiate operations on the Usage Collector |
| `cpt-cf-usage-collector-actor-monitoring-system` | Read observability endpoints (`/health/*`, metrics scrape, watermark API) | Modifying usage records or configuration |

Authorization is enforced via the platform PDP (`authz-resolver`) on all read and write operations. Unauthenticated requests are rejected before any authorization check. Failures result in immediate rejection with no partial operation (`cpt-cf-usage-collector-principle-fail-closed`).

## 3. Operational Concept & Environment

No module-specific environment constraints beyond project defaults.

## 4. Scope

### 4.1 In Scope

- Usage record ingestion from platform services
- Counter and gauge metric semantics
- Per-tenant usage attribution, PDP-authorized at emit time
- Per-subject (user, service account) usage attribution derived from SecurityContext
- Per-resource usage attribution
- Ingestion authorization via the platform PDP
- Per-source, per-tenant rate limiting to prevent outbox flooding
- Idempotency via client-provided keys
- At-least-once delivery of records from sources to the Usage Collector
- Pluggable storage backend (ClickHouse, TimescaleDB)
- Query API for aggregated usage data (SUM, COUNT, MIN, MAX, AVG) with time-range and grouping
- Tenant isolation on all read and write operations
- Per-record extensible metadata (optional, opaque to the Usage Collector)
- Configurable retention policies (global, per-tenant, per-usage-type) with automated enforcement
- Backfill API for retroactive submission of historical usage data
- Usage type validation and custom measuring unit registration

### 4.2 Out of Scope

- **Business Logic**: Pricing, rating, billing rules, invoice generation, quota enforcement decisions — responsibility of downstream consumers
- **Multi-Region Replication**: Deferred to future phase

## 5. Functional Requirements

### 5.1 Usage Ingestion

#### SDK Usage Record Ingestion

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-ingestion`

The system **MUST** accept usage records from in-process platform services via the Usage Collector SDK. Each usage record represents a single measurement of resource consumption attributed to a tenant.

- **Rationale**: Centralizing usage ingestion ensures all downstream consumers operate on the same data.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

#### REST API Usage Record Ingestion

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-rest-ingestion`

The system **MUST** expose a REST API endpoint for usage record submission, enabling remote CyberFabric nodes and external non-CyberFabric systems to submit records over HTTP using OAuth2 client credentials. The REST ingestion path **MUST** be subject to identical PDP authorization and validation rules as the SDK ingestion path.

For trusted CyberFabric forwarder modules (e.g., `usage-collector-rest-client`), a two-level authorization model applies: PDP authorization is enforced at the source side against the original caller's SecurityContext before any record enters the forwarder's outbox; the gateway REST endpoint then authenticates the forwarder via service-to-service bearer token and applies a gateway-level PDP check against the forwarder's service identity. Both checks must pass before any record is accepted for storage.

For external non-CyberFabric systems calling the REST endpoint directly, the gateway PDP check against the caller's OAuth2 identity is the sole authorization boundary.

- **Rationale**: Remote and external sources that cannot embed the SDK — such as remote collection agents forwarding records from distributed locations, or third-party systems integrated with the platform — must be able to contribute usage data through the same authoritative store without requiring a SDK dependency. The two-level model for CyberFabric forwarders preserves per-caller authorization at the source while maintaining gateway-level enforcement against the forwarder's service identity.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

#### Idempotent Ingestion

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-idempotency`

The system **MUST** support client-provided idempotency keys. When an idempotency key is provided, duplicate submissions with the same key **MUST** be silently deduplicated (no error, no duplicate record). Counter records **MUST** include an idempotency key; the system **MUST** reject counter records submitted without one. When no idempotency key is provided for a gauge record, the record is accepted as-is without deduplication.

- **Rationale**: At-least-once delivery semantics can produce duplicate submissions; deduplication prevents incorrect aggregations. For counter metrics, a retry of a keyless delta delivery inflates the accumulated total without any means of detection or correction — requiring an idempotency key on every counter emission eliminates this data integrity risk at the source. Gauge metrics lack a meaningful cumulative total, so duplicate gauge readings introduce at most transient noise rather than a systematic data integrity failure.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

#### Per-Record Extensible Metadata

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-fr-record-metadata`

The system **MUST** support an optional, extensible metadata field on each usage record, allowing usage sources to attach arbitrary key-value properties as a JSON object. The system **MUST** persist metadata as-is and return it in query results without interpretation. The system **MUST** enforce a configurable maximum size limit on the metadata field (default 8 KB per record) and **MUST** reject records exceeding the limit with an actionable error.

The system **MUST NOT** index, aggregate, or interpret metadata contents — metadata is opaque to the Usage Collector. Downstream consumers (billing, reporting, analytics) are responsible for extracting and processing metadata fields according to their own domain logic.

- **Rationale**: Different usage sources need to attach context-specific properties to usage records (e.g., LLM model name, token type, request category, geographic region) that enable downstream reporting and analytics. Storing metadata per-record at ingestion time avoids the need to correlate usage records with external context stores.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`, `cpt-cf-usage-collector-actor-platform-developer`

### 5.2 Metric Semantics

#### Counter Metric Semantics

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-counter-semantics`

The system **MUST** enforce counter semantics: sources submit non-negative delta values representing consumption since their last report. The system **MUST** reject counter records with negative values. The system **MUST** accumulate submitted deltas into a persistent, monotonically increasing total per (tenant, metric) tuple.

- **Rationale**: Delta-based reporting decouples the source's internal state from the Usage Collector's persistent totals. Sources never report cumulative values, so process restarts and counter resets in the source are transparent — a restart simply results in the next emission starting from zero again, which is valid.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

#### Gauge Metric Semantics

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-gauge-semantics`

The system **MUST** support gauge metrics representing point-in-time values. Gauge records **MUST** be stored as-is without monotonicity constraints or delta accumulation.

- **Rationale**: Gauges represent instantaneous measurements (e.g., current active connections, memory usage in bytes) that naturally fluctuate and have no meaningful cumulative total.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

### 5.3 Delivery Guarantee

#### At-Least-Once Delivery

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-delivery-guarantee`

The system **MUST** guarantee at-least-once delivery of usage records from sources to the Usage Collector. Once a usage source has emitted a record via the SDK, that record **MUST** eventually reach the Usage Collector and be available for querying, even if the Usage Collector is temporarily unavailable at the time of emission. The system **MUST** surface permanently undeliverable records via operational monitoring.

- **Rationale**: Usage data is billing-critical; loss of emitted records is unacceptable.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

### 5.4 Attribution & Isolation

#### Tenant Attribution

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-tenant-attribution`

The system **MUST** attribute all usage records to a tenant supplied by the caller in the request. The system **MUST** authorize the caller's tenant attribution via the platform PDP before any record is accepted, verifying that the authenticated caller is permitted to emit records for the specified tenant. This covers both same-tenant emission and parent→subtenant scenarios (e.g., a platform-level metering agent collecting usage for resources owned by its subtenants). The gateway **MUST** independently validate tenant attribution on ingest as a defense-in-depth check.

- **Rationale**: Requiring callers to supply the target tenant explicitly supports all emission scenarios — including remote forwarders and external systems that emit records on behalf of multiple tenants — through a single uniform path. PDP authorization remains the security boundary enforcing which tenants a given caller is permitted to report for.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

#### Resource Attribution

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-resource-attribution`

Every usage record **MUST** be attributed to a specific resource instance within a tenant, identified by a resource ID and resource type. Resource attribution is mandatory; the system **MUST** reject records that omit either field.

- **Rationale**: Per-resource attribution enables granular billing, per-resource quota enforcement, and detailed usage analysis at the resource level. Mandatory attribution ensures downstream consumers always have a resource scope to aggregate and filter on, without needing to handle the absence of this field.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

#### Subject Attribution

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-subject-attribution`

The system **MUST** support attributing usage records to a subject (user, service account, or other principal) within a tenant, identified by a subject ID and an optional subject type. The wire shape is a single nested `subject` object: `{ "subject": { "id": "...", "type": "user" } }`, or the `subject` key is absent. The inner `type` field is itself optional, but `id` is required whenever `subject` is present (the "type without id" payload is not expressible). Subject attribution is **optional** per usage record to accommodate system-level resource consumption not attributable to a specific subject (e.g., background jobs where per-user attribution is not meaningful). The system **MUST** accept usage records where the `subject` field is absent. When `subject` is present, the system **MUST** validate it against PDP authorization; when it is absent, PDP subject validation is skipped.

- **Rationale**: Per-subject attribution enables chargeback, per-subject quota enforcement, and visibility into which principals drive consumption within a tenant. Deriving subject identity server-side from SecurityContext prevents spoofing.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`
- **Data Classification**: Subject IDs stored by the Usage Collector are opaque internal platform identifiers issued and managed by the platform identity layer (SecurityContext). They are not directly identifying natural persons within this module. PII management responsibilities belong to the platform identity layer; the Usage Collector stores and processes only these opaque identifiers. See §6.3 NFR Exclusions for the Privacy by Design applicability statement.

#### Tenant Isolation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-tenant-isolation`

The system **MUST** enforce strict tenant isolation on all read and write operations, ensuring usage data is never accessible across tenants. The system **MUST** fail closed on authorization failures.

- **Rationale**: Tenant data isolation is a security and compliance requirement.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`, `cpt-cf-usage-collector-actor-usage-consumer`

#### Ingestion Authorization

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-ingestion-authorization`

The system **MUST** authorize each usage record emission before it is persisted. The security boundary for ingestion authorization is the PDP check on the caller's authenticated identity against the target tenant and resource — the system **MUST** verify the caller is permitted to emit records for the specified tenant and resource before any record is accepted.

The `module` field in a usage record identifies the originating source for attribution and metric validation purposes. Its semantics differ by ingestion path:

- **SDK ingestion**: module identity is set in-process at SDK client construction time and is implicitly trusted as the emitting service's own identity.
- **REST API ingestion**: the `module` field is caller-supplied attribution metadata, accepted on trust. It is used to validate that the emitted metric is registered for the claimed module type (a data quality check), but it is not a security authorization principal — the PDP check on tenant and resource is the authoritative security enforcement.

The system **MUST** validate that the emitted metric is registered for the module type claimed in the record, rejecting records that reference metrics not associated with the claimed module. Authorization failures **MUST** be surfaced immediately to the caller before any domain operation is committed. The system **MUST** fail closed: unauthorized records are never persisted, and there is no silent discard of denied emissions.

- **Rationale**: Decoupling module attribution from the authorization security boundary enables the REST ingestion path to support legitimate forwarding scenarios — such as a remote agent relaying records originally produced by multiple named source modules — without requiring per-module credentials. The PDP remains the authoritative enforcement point for what tenants and resources a caller may emit for. Metric validation against the claimed module type preserves data quality by ensuring records carry a plausible and registered module identity.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

### 5.5 Pluggable Storage

#### Pluggable Storage Backend

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-pluggable-storage`

The system **MUST** support pluggable storage backends. Each plugin provides persistence and query capabilities for its specific backend (ClickHouse, TimescaleDB). The operator selects the active backend via configuration.

- **Rationale**: Pluggable storage avoids lock-in and allows operators to choose the backend that fits their needs.
- **Actors**: `cpt-cf-usage-collector-actor-platform-operator`, `cpt-cf-usage-collector-actor-storage-backend`

### 5.6 Usage Query & Aggregation

#### Aggregated Usage Query

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-query-aggregation`

The system **MUST** provide an API for querying aggregated usage data. Queries **MUST** support:
- Filtering by tenant (mandatory, derived from SecurityContext), time range (mandatory), and optionally by usage type, subject (subject ID and subject type), resource (resource ID and resource type), and source
- Server-side aggregation functions: SUM, COUNT, MIN, MAX, AVG
- Grouping by any combination of: time bucket (e.g., hourly, daily), usage type, subject, resource, and source

The system **MUST** authorize each query via the platform PDP. The PDP decision determines whether the caller is permitted to query usage data; PDP-returned constraints are applied as additional query filters before execution. The system **MUST** fail closed on authorization failures.

- **Rationale**: Downstream consumers (billing, dashboards) need aggregated views without fetching and processing raw records. Filter and grouping dimensions enable billing breakdowns (e.g., tokens per model per day) and per-subject chargeback without requiring consumers to process raw records.
- **Actors**: `cpt-cf-usage-collector-actor-usage-consumer`, `cpt-cf-usage-collector-actor-tenant-admin`

#### Raw Usage Query

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-fr-query-raw`

The system **MUST** provide an API for querying raw usage records with cursor-based pagination. Queries **MUST** support filtering by tenant (mandatory, derived from SecurityContext), time range (mandatory), and optionally by usage type, subject (subject ID and subject type), and resource (resource ID and resource type).

The system **MUST** authorize each query via the platform PDP using decision and constraint enforcement, identical to the aggregation query path.

- **Rationale**: Some consumers need access to individual records for auditing, debugging, or dispute resolution.
- **Actors**: `cpt-cf-usage-collector-actor-usage-consumer`, `cpt-cf-usage-collector-actor-tenant-admin`

### 5.7 Retention Policy Management

#### Retention Policy Management

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-retention-policies`

The system **MUST** support configurable retention policies at three scopes: global (applies to all records), per-tenant, and per-usage-type. When multiple policies match a record, the most-specific scope wins: per-usage-type > per-tenant > global.

The system **MUST** enforce a global default retention policy. When no policy matches a record, the global default applies. The system **MUST** reject requests to delete the global default policy.

Retention enforcement **MUST** permanently delete expired records (hard delete). The `inactive` status is reserved for operator-initiated amendments and **MUST NOT** be used by retention enforcement.

- **Rationale**: Retention policies balance storage costs with compliance and operational needs. Different usage types may have different regulatory retention requirements. Explicit precedence rules and a mandatory global default ensure no record has undefined retention behavior.
- **Actors**: `cpt-cf-usage-collector-actor-platform-operator`

### 5.8 Backfill & Amendment

#### Backfill API

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-fr-backfill-api`

The system **MUST** provide a backfill API that allows operators to bulk-insert historical usage records for a specific time range (scoped to a single tenant and usage type). Backfill is purely additive: existing records in the target range are not modified.

Real-time ingestion **MUST** continue uninterrupted during a backfill operation. Real-time events that arrive in the target range during or after the backfill are recorded normally and are not affected by the backfill. The backfill API **MUST** be isolated from the real-time ingestion path with independent rate limits and lower processing priority to prevent backfill operations from degrading real-time ingestion performance.

- **Rationale**: When usage data is lost due to outages, pipeline failures, or misconfigured sources, operators need a mechanism to retroactively submit the missing records. Keeping backfill and real-time ingestion independent avoids any interruption to ongoing usage reporting.
- **Actors**: `cpt-cf-usage-collector-actor-platform-operator`, `cpt-cf-usage-collector-actor-usage-source`

#### Individual Event Amendment

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-fr-event-amendment`

The system **MUST** support amending individual usage events (updating properties except tenant ID and timestamp) and deactivating individual events (marking them as inactive while retaining them for audit). Downstream consumers **MUST** be able to distinguish active from inactive records when querying.

- **Rationale**: Not all corrections require full timeframe backfill. Individual event amendments handle cases like incorrect resource attribution or value errors on specific events.
- **Actors**: `cpt-cf-usage-collector-actor-platform-operator`

#### Backfill Time Boundaries

- [ ] `p3` - **ID**: `cpt-cf-usage-collector-fr-backfill-boundaries`

The system **MUST** enforce configurable time boundaries for backfill operations: a maximum backfill window (default 90 days) beyond which backfill requests are rejected, and a future timestamp tolerance (default 5 minutes) to account for clock drift. Backfill requests exceeding the maximum window **MUST** require elevated authorization.

- **Rationale**: Unbounded backfill creates risks for data integrity and billing accuracy. Time boundaries constrain the blast radius of backfill operations while allowing legitimate corrections.
- **Actors**: `cpt-cf-usage-collector-actor-platform-operator`


#### Metadata Exposure

- [ ] `p3` - **ID**: `cpt-cf-usage-collector-fr-metadata-exposure`

The system **MUST** expose per-source and per-tenant metadata — including event counts, latest event timestamps (watermarks), and ingestion statistics — via API, enabling external reconciliation and observability systems to detect gaps and perform integrity checks.

- **Rationale**: While reconciliation logic is out of scope for the Usage Collector, exposing the raw metadata needed for gap detection enables external systems to build reconciliation workflows.
- **Actors**: `cpt-cf-usage-collector-actor-platform-operator`, `cpt-cf-usage-collector-actor-monitoring-system`

### 5.9 Usage Type System

#### Usage Type Validation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-type-validation`

The system **MUST** validate all usage records against their registered type schema before any record is accepted for delivery, rejecting invalid records immediately with actionable error messages.

- **Rationale**: Schema validation prevents corrupt or malformed data from entering the system, ensuring downstream consumers operate on well-structured data. Immediate rejection surfaces errors to the caller before any domain operation is committed.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`, `cpt-cf-usage-collector-actor-types-registry`

#### Custom Measuring Unit Registration

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-custom-units`

The system **MUST** allow platform operators to register custom measuring units via API without code changes or service redeployment.

Primary use cases: AI/LLM token metering (input/output tokens, custom credit units), compute metering (vCPU-hours, GPU-hours), API request metering (calls by tenant and endpoint), storage metering (GB-hours across tiers), and network transfer (bytes ingress/egress).

When a custom measuring unit is registered, the platform operator **MUST** also configure the authorization policies that declare which sources are permitted to emit records of this type.

- **Rationale**: New resource types (AI tokens, GPU-hours) must be meterable without service redeployment. Custom unit registration enables rapid onboarding of new usage types as the platform evolves.
- **Actors**: `cpt-cf-usage-collector-actor-platform-operator`

### 5.10 Audit Events

#### Audit Event Emission

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-fr-audit`

The system **MUST** emit a structured audit event to the platform `audit_service` for every operator-initiated write operation (backfill, amendment, deactivation). Each event **MUST** include the operator identity, tenant, timestamp, operation type, operation-specific context, and a mandatory operator-supplied justification. The Usage Collector **MUST NOT** store audit data locally — audit storage, retention, and deletion semantics are owned by `audit_service`.

Routine ingestion and read operations are not audited via `audit_service`.

- **Rationale**: Operator-initiated mutations are high-risk changes to billing-critical data. Emitting to the platform `audit_service` provides a consistent, centralized audit trail across all platform modules.
- **Actors**: `cpt-cf-usage-collector-actor-platform-operator`

### 5.11 Rate Limiting

#### Emission Rate Limiting

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-rate-limiting`

The system **MUST** enforce rate limits on usage record emission at two levels:

**Source-level (enforced at the SDK, before any outbox INSERT)**: A configurable per-source emission quota limits the total number of records a source module may emit across all tenants within a rate limit window. The emitter factory's `.authorize(...)` step fetches the current source-level quota snapshot before any DB transaction opens; `enqueue()` evaluates it in-memory and rejects the emission if the quota is exhausted. This provides low-latency, source-side flood prevention without requiring per-tenant quota pre-fetching at emit time.

**Per-tenant (enforced at the gateway on ingest)**: A configurable per-(source module, tenant) quota limits the number of records a source may deliver for any single tenant within a rate limit window. The gateway enforces this on the inbound `create_usage_record()` call, after the record's `tenant_id` is known. This provides granular per-tenant protection at delivery time.

Rate limits are configured by the platform operator. An operator **MAY** configure a default quota that applies when no explicit entry exists; if neither applies, no rate limit is enforced.

When a source exceeds its configured quota, the emission or delivery **MUST** be rejected with an actionable error. The system **MUST** surface rate-limit rejections via operational monitoring.

Rate limit enforcement **MUST NOT** degrade ingestion latency for emissions within quota.

Rate limit enforcement provides a best-effort flood prevention guarantee: individual emissions that observe an exhausted quota are rejected, but a small number of concurrent emissions may slip past the limit under high concurrency. This is acceptable for flood prevention; hard-counted accounting is the responsibility of downstream consumers.

- **Rationale**: A misbehaving or misconfigured source can flood the delivery queue or a single tenant's records, degrading ingestion for all sources and overwhelming downstream processing. Source-level limits with immediate SDK-side rejection protect the system without requiring pre-fetching of per-tenant quota state for sources that emit for many tenants. Gateway-side per-tenant limits provide granular protection once the target tenant is known.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`, `cpt-cf-usage-collector-actor-platform-operator`

## 6. Non-Functional Requirements

### 6.1 Module-Specific NFRs

#### Query Latency

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-query-latency`

Aggregation queries over a 30-day range for a single tenant **MUST** complete within 500ms at p95.

- **Threshold**: 500ms p95 for 30-day single-tenant aggregation
- **Rationale**: Interactive dashboard and billing queries need timely responses.
- **Architecture Allocation**: See DESIGN.md §3.7 Counter Accumulation. Meeting this threshold at production record volumes requires storage plugins to maintain pre-aggregated acceleration structures (e.g., ClickHouse `AggregatingMergeTree` view or TimescaleDB continuous aggregate). These structures are eventually consistent with the delta records table; the maximum refresh lag is an operational parameter documented per plugin.

#### High Availability

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-availability`

The system **MUST** maintain 99.95% monthly availability for usage ingestion endpoints.

- **Threshold**: 99.95% uptime per calendar month
- **Rationale**: Usage collection is on the critical path for all billable operations.
- **Architecture Allocation**: See DESIGN.md

#### Ingestion Throughput

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-throughput`

The system **MUST** sustain ingestion of at least 10,000 usage records per second under normal operation.

- **Threshold**: ≥ 10,000 records/sec sustained
- **Rationale**: High-volume services (LLM Gateway, API Gateway) generate significant event throughput; the ingestion path must not become a bottleneck.
- **Architecture Allocation**: See DESIGN.md

#### Ingestion Latency

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-ingestion-latency`

The system **MUST** complete usage record ingestion within 200ms at p95 under normal load.

- **Threshold**: p95 ≤ 200ms
- **Rationale**: Low ingestion latency prevents blocking in usage source services.
- **Architecture Allocation**: See DESIGN.md

#### Workload Isolation

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-nfr-workload-isolation`

The system **MUST** ensure that retention enforcement jobs and aggregation query workloads do not degrade ingestion latency. These workloads **MUST** be isolated from the ingestion path such that concurrent execution maintains ingestion p95 latency within the `cpt-cf-usage-collector-nfr-ingestion-latency` threshold.

- **Threshold**: Ingestion p95 latency remains ≤ 200ms during concurrent query and retention operations
- **Rationale**: Retention and aggregation are batch or analytical workloads that can compete for storage resources with the latency-sensitive ingestion path.
- **Architecture Allocation**: See DESIGN.md

#### Authentication Required

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-authentication`

The system **MUST** require authentication for all API operations. Unauthenticated requests **MUST** be rejected before any operation is performed.

- **Threshold**: Zero unauthenticated API access
- **Rationale**: Usage data is billing-sensitive; unauthenticated access is a security violation.
- **Architecture Allocation**: See DESIGN.md

#### Authorization Enforcement

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-nfr-authorization`

The system **MUST** enforce authorization for all read and write operations based on the caller's authenticated identity, tenant context, and usage type permissions.

- **Threshold**: Zero unauthorized data access or write
- **Rationale**: Authorization prevents unauthorized usage data manipulation and cross-tenant data leakage.
- **Architecture Allocation**: See DESIGN.md

#### Horizontal Scalability

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-nfr-scalability`

The system **MUST** scale horizontally to handle increased ingestion and query load without architectural changes.

- **Threshold**: Linear throughput scaling with added instances
- **Rationale**: Usage volume grows with platform adoption; vertical scaling is insufficient for sustained growth.
- **Architecture Allocation**: See DESIGN.md

#### Storage Fault Tolerance

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-nfr-fault-tolerance`

Once a usage record has been durably captured by the delivery mechanism, the system **MUST** ensure it is eventually persisted to the storage backend and available for querying, even under storage backend failures. The system **MUST** buffer and retry persistence operations during storage outages without data loss.

- **Threshold**: Zero data loss for durably captured records during storage backend failures
- **Rationale**: Storage outages must not result in lost usage data for records already accepted by the delivery mechanism.
- **Architecture Allocation**: See DESIGN.md

#### Recovery Time

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-recovery`

The system **MUST** resume normal ingestion and make all durably captured records available for querying within 15 minutes of storage backend recovery.

- **Threshold**: RTO ≤ 15 minutes from storage backend recovery
- **Rationale**: Bounded recovery time ensures downstream billing and quota systems are not blocked for extended periods after a storage outage.
- **Architecture Allocation**: See DESIGN.md

#### Recovery Point Objective

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-rpo`

The system **MUST** guarantee zero data loss for all usage records that have been durably accepted by the SDK (`emit()` returned `Ok`). A record is durable once it is committed to the source's local outbox within the caller's database transaction. No committed record may be lost due to storage backend failures, gateway restarts, or dispatcher crashes.

- **Threshold**: RPO = 0 for committed records (zero committed data loss)
- **Rationale**: Usage records are billing-critical; any committed record that is silently lost represents an unrecoverable billing gap. The transactional outbox ensures that a record either commits to the source's local DB (and will eventually be delivered) or is never considered accepted.
- **Architecture Allocation**: See DESIGN.md

#### Configurable Retention Range

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-retention`

The system **MUST** support retention periods from 7 days to 7 years, selectable per usage type and per tenant.

- **Threshold**: Configurable retention from 7 days to 7 years
- **Rationale**: Different usage types have different compliance and operational retention requirements; a fixed retention period is insufficient.
- **Architecture Allocation**: See DESIGN.md

#### Graceful Degradation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-graceful-degradation`

The system **MUST** continue accepting and persisting usage records even if downstream consumers (billing, monitoring) are unavailable.

- **Threshold**: Zero ingestion failures due to downstream consumer unavailability
- **Rationale**: Usage collection must not be blocked by consumer outages; the collector is the source of truth and must remain operational independently.
- **Architecture Allocation**: See DESIGN.md

### 6.2 Data Governance

**Data Steward**: Platform Engineering team owns the usage data schema, ingestion policies, and retention rules for the Usage Collector. The data steward is responsible for maintaining usage type definitions, authorizing metric namespaces per source module, and setting global retention policy.

**Data Classification**: Usage records contain numeric measurements, metric names, timestamps, and tenant-scoped opaque identifiers (tenant ID, subject ID, resource ID). No natural-person PII is stored directly. Records are classified as **Platform Operational Data** — internal, business-sensitive, restricted to authenticated platform actors.

**Data Ownership**:

| Data | Owner | Custodian |
|------|-------|-----------|
| Usage records (metric values, timestamps) | The tenant identified by `tenant_id` in each record | Usage Collector module (storage) |
| Retention policy configuration | Platform Operator | Usage Collector module (enforcer) |
| Usage type schemas and measuring unit definitions | Platform Engineering | types-registry module |
| Audit events for operator-initiated writes | Platform Engineering | audit_service module |
| Outbox rows (pre-delivery records) | Source module (until delivered) | modkit-db infrastructure |

**Retention**: Governed by configurable retention policies (global, per-tenant, per-usage-type) as defined in `cpt-cf-usage-collector-fr-retention-policies`. The global default policy MUST be set by the platform operator at deployment time; it cannot be deleted.

**Deletion**: Retention enforcement performs hard deletes — records are permanently removed from the storage backend when their retention period expires. The deletion is not reversible. Operator-initiated deactivation sets `status = inactive` but retains the record; permanent deletion is only via retention enforcement.

### 6.3 NFR Exclusions

The following commonly applicable NFR categories are not applicable to this module:

- **Safety (ISO/IEC 25010:2023 §4.2.9)**: Not applicable — the Usage Collector is a server-side data API with no physical interaction, no safety-critical operations, and no ability to cause harm to people, property, or the environment.
- **Accessibility and Usability (UX)**: Not applicable — the Usage Collector exposes no user-facing UI. It provides a developer SDK and a server-side API consumed exclusively by platform services and internal systems.
- **Internationalization / Localization**: Not applicable — the module exposes no user-facing text, labels, or locale-sensitive output.
- **Privacy by Design (GDPR Art. 25)**: Not applicable as a standalone module requirement. Subject IDs stored by the Usage Collector are opaque internal platform identifiers; PII management is the responsibility of the platform identity layer. See note in §5.4 (Subject Attribution).
- **Regulatory Compliance (GDPR, HIPAA, PCI DSS, SOX)**: Not applicable as a standalone module requirement — this is an internal platform infrastructure module. This module handles no payment card data (PCI DSS N/A), no healthcare records (HIPAA N/A), and no financial reporting data (SOX N/A). Platform-level regulatory obligations are governed at the platform level.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Usage Collector SDK Trait

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-interface-sdk-client`

- **Type**: Programmatic SDK (language-native client interface)
- **Stability**: unstable (v0)
- **Description**: Public interface for emitting usage records and querying aggregated data; technical interface type and crate naming defined in DESIGN.md
- **Breaking Change Policy**: Unstable during initial development; will stabilize in future version

#### REST Ingestion API

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-interface-rest-ingestion`

- **Type**: HTTP REST API
- **Stability**: unstable (v0)
- **Description**: HTTP endpoint for submitting usage records from remote or external sources; authentication via OAuth2 client credentials; request and response schema defined in DESIGN.md
- **Breaking Change Policy**: Unstable during initial development; will stabilize in future version

### 7.2 External Integration Contracts

#### Storage Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-contract-storage-plugin`

- **Direction**: required from plugin implementor
- **Protocol/Format**: Rust trait implemented by each storage backend plugin
- **Compatibility**: Plugin contract versioned with the module; plugins must match the module's major version

## 8. Use Cases

#### Emit Usage Records

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-usecase-emit`

**Actor**: `cpt-cf-usage-collector-actor-usage-source`

**Preconditions**:
- Actor is authenticated with a valid SecurityContext containing tenant identity
- A scoped SDK client has been initialized with the source module's identity
- Authorization policies are configured declaring which usage types the source module is permitted to emit

**Main Flow**:
1. Usage source initializes a scoped SDK client, providing its module identity as the emission scope
2. Usage source emits usage records via the scoped client
3. The system checks the emission: verifies the source module is authorized to report the target usage types and that any attributed resource and subject fall within its authorized scope; validates the record against the registered usage type schema; and checks the emission count against the configured rate limit for the (source module, tenant) pair. Any failure is returned immediately to the caller before any record is accepted for delivery.
4. Records are durably captured with at-least-once delivery guarantee
5. Records become available for querying in the Usage Collector

**Postconditions**:
- Authorized, valid, within-quota records are persisted in the storage backend and available for aggregation queries
- Duplicate records (by idempotency key) are silently ignored

**Alternative Flows**:
- **Authorization denied**: System returns an error immediately; no record is accepted for delivery
- **Schema invalid**: System returns an actionable error immediately; no record is accepted for delivery
- **Rate limit exceeded**: System returns an actionable error immediately; no record is accepted for delivery

#### Query Aggregated Usage

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-usecase-query-aggregated`

**Actor**: `cpt-cf-usage-collector-actor-usage-consumer`, `cpt-cf-usage-collector-actor-tenant-admin`

**Preconditions**:
- Actor is authenticated with a valid SecurityContext

**Main Flow**:
1. Consumer sends an aggregation query specifying time range, aggregation function, and grouping
2. System derives tenant ID from SecurityContext
3. System returns aggregated results scoped to the tenant

**Postconditions**:
- Consumer receives aggregated usage data scoped to their tenant

**Alternative Flows**:
- **No data in range**: System returns empty result set (not an error)

#### Register Custom Usage Type

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-usecase-configure-unit`

**Actor**: `cpt-cf-usage-collector-actor-platform-operator`

**Preconditions**:
- Actor is authenticated with a valid SecurityContext with operator-level permissions
- The usage type name is unique in the Types Registry

**Main Flow**:
1. Operator defines the usage type: a unique name, the metric kind (`counter` or `gauge`), and a unit label (e.g. `bytes`, `requests`, `vCPU-hours`)
2. Operator submits the definition via the API
3. System validates the definition against schema constraints
4. System registers the usage type with the Types Registry
5. Operator configures authorization policies declaring which source modules are permitted to emit records of this type
6. System confirms successful registration

**Postconditions**:
- The new usage type is immediately available for ingestion; sources can emit records using its name
- Authorization policies are in effect; unauthorized sources are rejected when attempting to emit records of this type

**Alternative Flows**:
- **Duplicate name**: System rejects registration with an actionable error; no type is created
- **Invalid definition**: System returns a validation error; no type is created

## 9. Acceptance Criteria

- [ ] Usage records emitted by sources are eventually available for querying (at-least-once delivery)
- [ ] Duplicate records with the same idempotency key result in a single stored record
- [ ] Counter records with negative values are rejected at ingestion
- [ ] Gauge records are stored as-is without monotonicity constraints
- [ ] Incoming usage records include an explicit tenant attribute; the platform PDP validates that the authenticated caller is authorized to emit records for the specified tenant before the record is accepted, and the gateway independently validates tenant attribution on ingest as a defense-in-depth check
- [ ] Every usage record includes resource attribution (resource ID and type); records without either field are rejected
- [ ] Usage records can optionally be attributed to a subject (subject ID and type); when supplied with PDP authorization, subject is accepted from the request payload; when absent, PDP subject validation is skipped
- [ ] Authorization failures are surfaced immediately to the caller; no record is persisted on denial
- [ ] A `UsageEmitter` handle older than `MAX_AUTH_AGE` (30 seconds) is rejected by `enqueue()` with a `UsageEmitterError::Unauthenticated` (built via `UsageEmitterError::unauthenticated()` with reason "emit authorization has expired") error before any record is accepted for delivery
- [ ] A source module that exceeds its configured emission quota for a given tenant within the current rate limit window is rejected with an actionable error before any record is accepted for delivery
- [ ] Rate limit configuration can be set per (source module, tenant) pair; a default per-source quota applies when no tenant-specific entry exists
- [ ] Rate limit enforcement does not degrade ingestion latency for emissions within quota
- [ ] Tenant isolation is enforced: a query for tenant A never returns tenant B's data
- [ ] Aggregation queries (SUM, COUNT, MIN, MAX, AVG) return correct results for a given tenant and time range, with correct filtering by usage type, subject, and resource when specified
- [ ] Aggregation results can be grouped by any combination of time bucket, usage type, subject, resource, and source
- [ ] Raw usage queries support filtering by usage type, subject, and resource in addition to tenant and time range
- [ ] Query authorization is enforced via PDP decision and constraint enforcement; unauthorized queries are rejected and PDP-returned constraints narrow the result scope
- [ ] The module works with both ClickHouse and TimescaleDB plugins without code changes to the core module
- [ ] Metadata attached to a usage record is persisted as-is and returned in query results without modification
- [ ] Usage records with metadata exceeding the configured size limit are rejected with an actionable error
- [ ] Retention policies can be configured at global, per-tenant, and per-usage-type granularity; the global default policy cannot be deleted
- [ ] When multiple retention policies match a record, the most-specific scope is applied (per-usage-type > per-tenant > global)
- [ ] Retention enforcement permanently deletes expired records; expired records are not marked inactive
- [ ] Real-time ingestion continues uninterrupted during a backfill operation
- [ ] Backfill inserts historical records without modifying existing records in the target range
- [ ] Backfill requests exceeding the maximum window require elevated authorization
- [ ] Individual usage events can be amended or deactivated; inactive events remain queryable for audit
- [ ] Per-source and per-tenant metadata (event counts, latest event timestamps, ingestion statistics) is accessible via API and enables external systems to detect data gaps
- [ ] All usage records are validated against their registered type schema before any record is accepted for delivery; invalid records are rejected immediately with actionable error messages
- [ ] Custom measuring units can be registered via API without code changes or service redeployment
- [ ] The system maintains 99.95% monthly availability for ingestion endpoints
- [ ] The system sustains ingestion of at least 10,000 records/sec under normal load without degradation
- [ ] Usage record ingestion completes within 200ms at p95 under normal load
- [ ] Aggregation queries over a 30-day range for a single tenant complete within 500ms at p95 under normal load
- [ ] Ingestion p95 latency remains ≤ 200ms during concurrent aggregation queries and retention enforcement workloads
- [ ] Every operator-initiated write operation (backfill, amendment, deactivation) emits a structured audit event to the platform `audit_service` with the required common fields and operation-specific context; justification is required and included in every such event
- [ ] All API operations require authentication; unauthenticated requests are rejected before any operation is performed
- [ ] Authorization is enforced on all read and write operations; unauthorized requests are rejected and no data is exposed or modified
- [ ] The REST ingestion endpoint (`cpt-cf-usage-collector-fr-rest-ingestion`) rejects requests from `cpt-cf-usage-collector-actor-usage-source` that carry no bearer token or an invalid/expired OAuth2 client-credentials token with HTTP 401; requests carrying a valid token proceed to PDP evaluation
- [ ] When a trusted CyberFabric forwarder (e.g., `usage-collector-rest-client`) acting as `cpt-cf-usage-collector-actor-usage-source` submits records via the REST endpoint (`cpt-cf-usage-collector-fr-rest-ingestion`), the source-side PDP check against the original caller's SecurityContext must pass before any record enters the forwarder's outbox, and the gateway-side PDP check against the forwarder's service identity must also pass; a failure at either level causes the record to be rejected
- [ ] When an external non-CyberFabric `cpt-cf-usage-collector-actor-usage-source` calls the REST ingestion endpoint (`cpt-cf-usage-collector-fr-rest-ingestion`) directly, only the gateway PDP check against the caller's OAuth2 identity is applied; records are accepted when that check passes and rejected otherwise
- [ ] Throughput scales linearly as additional instances are added
- [ ] Durably captured records are eventually persisted to the storage backend and available for querying after storage backend recovery
- [ ] Retention periods can be configured from 7 days to 7 years per usage type and per tenant
- [ ] Ingestion continues uninterrupted when downstream consumers (billing, monitoring) are unavailable

## 10. Dependencies

| Dependency                | Description                                       | Criticality |
| ------------------------- | ------------------------------------------------- | ----------- |
| modkit-db                 | Durable event persistence infrastructure          | p1          |
| SecurityContext           | Tenant and subject identity derivation            | p1          |
| authz-resolver            | Platform PDP for ingestion authorization          | p1          |
| ClickHouse or TimescaleDB | At least one storage backend must be available     | p1          |
| types-registry            | Usage type schema registration and validation      | p1          |
| audit_service             | Platform audit event ingestion                    | p1          |

## 11. Assumptions

| Assumption | Owner | Validation |
|------------|-------|------------|
| The modkit-db durable event persistence infrastructure is available as shared infrastructure | Platform Infrastructure | Verified at module bootstrapping; usage-collector module fails to start if modkit-db outbox table is not present |
| At least one storage backend plugin (ClickHouse or TimescaleDB) is deployed alongside the module | Platform Infrastructure / Operator | Verified at gateway startup via GTS plugin resolution; gateway fails readiness check if no active plugin resolves |
| Ingestion volume is moderate; client-side SDK batching is not needed initially | Platform Engineering | Revisit if `outbox_pending_rows` sustained backlog exceeds 10× normal throughput; SDK batching ADR to be authored if threshold is reached |
| The types-registry module is deployed and available for usage type schema registration and validation | Platform Engineering | Verified at gateway startup via health check; gateway fails readiness check if types-registry is unreachable |
| The platform `audit_service` is available to receive structured audit events | Platform Infrastructure | Audit emission failures are best-effort (non-blocking); sustained failures are surfaced via `usage_audit_emit_total{result="failed"}` alert |

## 12. Risks

| Risk                                                              | Impact                          | Mitigation                                                       |
| ----------------------------------------------------------------- | ------------------------------- | ---------------------------------------------------------------- |
| Delivery mechanism becomes a bottleneck under high ingestion volume | Increased latency, delivery lag | Monitor delivery backlog; scale in future phase                  |
| Raw `SUM` over high-cardinality counter records exceeds 500ms p95 query latency | Slow dashboard/billing queries  | Plugins SHOULD maintain pre-aggregated acceleration structures (DESIGN.md §3.7); eventual consistency with bounded refresh lag is the expected model; plugins that omit pre-aggregation must benchmark and confirm the NFR is met without it |
| Permanently failed deliveries accumulate without operator attention | Data gaps in storage backend    | Operational monitoring and alerting on failed delivery count      |
| Optimistic rate limit enforcement allows a small burst above quota under high concurrency | Momentary outbox overfill | Acceptable for flood prevention; burst magnitude bounded by concurrent emission degree; monitoring surfaces sustained limit violations |

## 13. Open Questions

No open questions.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Features**: [features/](./features/)
