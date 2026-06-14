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
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Gear-Specific Environment Constraints](#31-gear-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Usage Ingestion](#51-usage-ingestion)
  - [5.2 UsageType Semantics](#52-usagetype-semantics)
  - [5.3 Attribution & Isolation](#53-attribution--isolation)
  - [5.4 Pluggable Storage](#54-pluggable-storage)
  - [5.5 Usage Query & Aggregation](#55-usage-query--aggregation)
  - [5.6 Corrections (Event Deactivation & Usage Compensation)](#56-corrections-event-deactivation--usage-compensation)
  - [5.7 Usage Types](#57-usage-types)
  - [5.8 Data Classification](#58-data-classification)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
  - [7.3 Endpoints Summary](#73-endpoints-summary)
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

A usage metering gear for collecting usage records from platform services and providing aggregated usage data to clients. The Usage Collector is the centralized product surface for platform usage data: it accepts usage records, retains them durably, and serves raw and aggregated views to downstream consumers.

### 1.2 Background / Problem Statement

Platform services need a centralized place to report resource consumption (API calls, AI tokens, storage bytes, compute hours) so that downstream systems (billing, quota reporting, dashboards) can operate on consistent data. Without a central usage gear, each consumer implements its own collection logic, leading to inconsistent data, duplicated effort, and no single source of truth.

The Usage Collector addresses this by accepting usage records from calling gears and providing a query/aggregation API to consumers. Business logic (pricing, billing rules, invoice generation, quota enforcement decisions) remains the responsibility of downstream consumers.

### 1.3 Goals (Business Outcomes)

- **Centralized metering**: All platform services that measure resource consumption report to a single authoritative store, eliminating per-service tracking implementations and data inconsistencies across the platform.
- **Operator self-service for new UsageTypes**: Platform operators can register new billable UsageTypes (e.g., GPU hours, custom credit units) via API without code changes or service redeployment, supporting rapid product iteration.
- **Downstream consumers need no aggregation layer**: Billing, quota enforcement, and dashboard systems obtain aggregated usage views directly from the Usage Collector within interactive latency bounds, without maintaining their own aggregation infrastructure.
- **Developer integration efficiency**: Platform developers can integrate a service with the SDK or REST API using published examples and receive actionable validation errors during ingestion.
- **Operator support readiness**: Platform operators can diagnose common ingestion, authorization, UsageType lifecycle, and storage-extension readiness problems using self-service documentation and standard service health information.

**Success Metrics**:

| Goal                                           | Measurable Success Criterion                                                                                                                                                                                    | Baseline                                                                                                                | Target                                                                                                                           | Timeframe                                                                                             |
| ---------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| Centralized metering                           | Existing platform services with billable operations integrated with Usage Collector as the authoritative usage source                                                                                           | No authoritative platform-wide usage source; billable services use per-service or consumer-specific tracking            | 100% of existing billable platform services integrated; zero per-service custom metering implementations remain for launch scope | By first production deployment; verified again within 30 calendar days after launch                   |
| Operator self-service                          | Time to register a new billable UsageType and emit the first accepted record without code changes or service redeployment                                                                                       | New billable usage dimensions require service-specific coordination outside Usage Collector                             | ≤ 5 minutes from authorized API request to first accepted record for a valid UsageType                                           | Available at first production deployment and sustained in monthly release-readiness checks            |
| Downstream consumers need no aggregation layer | Registered launch consumers serve primary aggregation use cases through the Usage Collector query API                                                                                                           | Billing, quota, and dashboard consumers require separate aggregation paths or cannot use one authoritative query source | 0 downstream-maintained aggregation tables for launch-scope billing, quota, and dashboard use cases                              | By first production deployment; verified during the first 90 calendar days after launch               |
| Developer integration efficiency               | Platform developer can use SDK or REST examples to submit a valid usage record in a clean service integration                                                                                                   | No shared Usage Collector integration guide or sample flow exists                                                       | First successful ingestion in ≤ 30 minutes for a developer familiar with platform auth and tenant concepts                       | Documentation and examples ready before production release candidate                                  |
| Operator support readiness                     | Platform operator can identify the owner-facing cause category for common failures: authn/authz denial, unregistered UsageType, metadata limit rejection, storage-extension readiness, and query-latency breach | Troubleshooting depends on gear maintainer assistance and ad hoc log review                                           | ≥ 90% of sampled common failure cases resolved to a documented cause category without maintainer escalation                      | Runbook complete before production release candidate; sampled during each quarterly operations review |

### 1.4 Glossary

| Term                   | Definition                                                                                                                                                                                                                                                                                                                                                                                                          |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Usage Record           | A single data point representing resource consumption by a tenant, with a numeric value and a timestamp, attributed to a registered UsageType                                                                                                                                                                                                                                                                       |
| UsageType              | A registered, platform-global definition of something the Usage Collector measures; the semantics — counter or gauge — is a derived property carried by the UsageTypeGtsId prefix.                                                                                                                                                                                                                                  |
| Counter                | A UsageType semantics representing a non-negative delta since the last report (e.g., API calls in this batch). Counter records support cumulative usage totals netted via `SUM` across `usage` and `compensation` entries.                                                                                                                                                                                          |
| Gauge                  | A UsageType semantics representing a point-in-time value that can go up or down (e.g., current memory usage in bytes). Stored as-is without monotonicity constraints.                                                                                                                                                                                                                                               |
| Idempotency Key        | A client-provided identifier that makes at-least-once processing safe: an exact-equality re-submission under the same key is silently absorbed (no duplicate record), while a same-key submission whose content differs is surfaced as a conflict rather than silently dropped. The key is never reused for a different record (unbounded window).                                                                  |
| Usage Collector Plugin | A storage extension selected by operators to provide the persistence and query capability behind the Usage Collector                                                                                                                                                                                                                                                                                                |
| Record Metadata        | An optional, extensible JSON object attached to a usage record, allowing usage sources to include context-specific properties (e.g., LLM model name, token category, geographic region) that are opaque to the Usage Collector and interpreted by downstream consumers                                                                                                                                              |
| Deactivation           | An operator-initiated transition of an existing usage record's `status` from active to `inactive`. The record is retained for downstream reference and remains queryable but is distinguishable from active records by downstream consumers. Deactivation does not modify any other property of the record                                                                                                          |
| Compensation           | A counter-only correction primitive that partially reverses a previously reported usage record by SUM netting; defined in §5.6.                                                                                                                                                                                                                                                                                     |
| GTS                    | Global Type System — the platform type and identifier system used by registry/orchestration dependencies outside the Usage Collector PRD boundary                                                                                                                                                                                                                                                                   |
| PDP                    | Policy Decision Point — the platform authorization service that gates every operation in this PRD.                                                                                                                                                                                                                                                                                                                  |
| SecurityContext        | A platform-resolved structure carrying the authenticated caller's identity; supplied to the gear by the platform — never accepted from the payload.                                                                                                                                                                                                                                                               |
| Audit Trail            | The combination of platform gateway access logs, platform authentication and PDP decision logs, and platform audit infrastructure that records authentication, authorization, ingestion, query, and operator-write outcomes for non-repudiation and forensic purposes. The Usage Collector contributes correlation identifiers to this trail but does not host its own audit log in v1 ([§4.2](#42-out-of-scope))   |
| PII                    | Personally identifiable information — any information relating to an identified or identifiable natural person. Within the Usage Collector boundary the gear handles only opaque platform identifiers; resolution of those identifiers to natural persons is owned by the platform identity layer ([§5.3](#53-attribution-isolation) Subject Attribution)                                                         |
| SPI                    | Service Provider Interface — the storage-plugin extension contract; distinct from the SDK trait and the REST API.                                                                                                                                                                                                                                                                                                   |

## 2. Actors

### 2.1 Human Actors

#### Platform Operator

**ID**: `cpt-cf-usage-collector-actor-platform-operator`

- **Role**: Deploys and configures the usage collector gear, selects storage backend, monitors system health.
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

- **Role**: Any authenticated system that produces usage records.

#### Usage Consumer

**ID**: `cpt-cf-usage-collector-actor-usage-consumer`

- **Role**: Any system that queries aggregated usage data (e.g., billing system, quota enforcer, dashboard).

#### Storage Backend

**ID**: `cpt-cf-usage-collector-actor-storage-backend`

- **Role**: The underlying data store (e.g., ClickHouse or TimescaleDB) that persists usage records.

**Actor Permissions** (shared across human and system actors):

| Actor                                             | Permitted Operations                                                                                                                                                                                                                                                                                                                                                                    | Denied by Default                                                                                                                                                                                                                              |
| ------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cpt-cf-usage-collector-actor-platform-operator`  | Deactivate individual records; create and delete UsageTypes                                                                                                                                                                                                                                                                                                                             | Querying or modifying records belonging to any tenant without an explicit security context                                                                                                                                                     |
| `cpt-cf-usage-collector-actor-platform-developer` | Emit usage records for UsageTypes the calling gear is PDP-authorized to emit, within the calling gear's authorized tenant scope (the calling-gear identity is derived from the platform-resolved `SecurityContext`)                                                                                                                                                                                                                                                       | Emitting records for UsageTypes outside the calling gear's PDP-authorized set; attributing records to subjects or resources outside the authorized scope                                                                                      |
| `cpt-cf-usage-collector-actor-tenant-admin`       | Query aggregated and raw usage records scoped to their own tenant                                                                                                                                                                                                                                                                                                                       | Accessing usage data of any other tenant; invoking operator-only operations (deactivation, UsageType registration)                                                                                                                             |
| `cpt-cf-usage-collector-actor-usage-source`       | Emit usage records for registered UsageTypes; the scope of permitted target tenants, resources, and UsageTypes is enforced by the platform PDP at emit time — the caller must be PDP-authorized for the tenant supplied in the record (covering both same-tenant and parent→subtenant scenarios), the supplied resource, and the referenced UsageType, with the calling-gear identity carried in the platform-resolved `SecurityContext` | Emitting records attributed to tenants or resources outside the PDP-authorized scope; emitting records referencing UsageTypes outside the PDP-authorized set; emitting records referencing UsageTypes that are not registered |
| `cpt-cf-usage-collector-actor-usage-consumer`     | Query aggregated and raw usage data scoped to the authenticated tenant; subject to PDP constraint filters                                                                                                                                                                                                                                                                               | Accessing cross-tenant data; mutating usage records                                                                                                                                                                                            |
| `cpt-cf-usage-collector-actor-storage-backend`    | Receive and persist usage records forwarded by the gateway plugin; respond to query operations initiated by the plugin                                                                                                                                                                                                                                                                  | Direct access from any actor other than the authorized plugin instance                                                                                                                                                                         |

Authorization is enforced via the platform PDP (`authz-resolver`) on all read and write operations. Unauthenticated requests are rejected before any authorization check. Failures result in immediate rejection with no partial operation (fail-closed).

## 3. Operational Concept & Environment

### 3.1 Gear-Specific Environment Constraints

- The gear is stateless; all durable state lives in the operator-selected storage plugin.
- Deployment, observability, and storage-tier HA are governed by platform operations and the active plugin's deployment guide.

## 4. Scope

### 4.1 In Scope

- Usage record ingestion from platform services
- Counter and gauge UsageType semantics
- Per-tenant usage attribution, PDP-authorized at emit time
- Per-subject (user, service account) usage attribution, PDP-authorized at emit time
- Per-resource usage attribution
- Ingestion authorization via the platform PDP
- Idempotency via client-provided keys
- Pluggable storage backend selection
- Query API for aggregated usage data with time-range filtering and grouping
- Tenant isolation on all read and write operations
- Per-record metadata constrained by the UsageType's declared metadata key set
- Individual event deactivation with downstream visibility of active/inactive status
- UsageType registration (create, delete)
- Caller authentication is performed by the platform gateway upstream of the gear
- Delegated audit trail through platform gateway access logs and platform audit infrastructure, with gear-emitted correlation identifiers on every API operation
- Custodianship of tenant usage data under PDP-mediated read and write boundaries, including tenant-owner, operator-steward, and gear-custodian role distinctions

### 4.2 Out of Scope

- **Business Logic**: Pricing, rating, billing rules, invoice generation, quota enforcement decisions — responsibility of downstream consumers
- **Multi-Region Replication**: Deferred to future phase
- **Retention Policy Management**: out of scope for v1 (no gear-level retention enforcement); the unbounded idempotency-key obligation is preserved (see §5.1).
- **Dedicated Backfill Capability**: out of scope for v1; bulk historical import rides the normal ingestion path.
- **Individual Event Amendment**: Operator-initiated property updates to existing usage records are out of scope for phase 1 of the gear; covered in a later phase
- **Audit Events**: Structured audit-event emission to the platform `audit_service` for operator-initiated writes is out of scope for phase 1 of the gear; covered in a later phase
- **Rate Limiting**: Per-caller-gear and per-(caller, tenant) ingestion quotas and rate-limit enforcement are out of scope for phase 1 of the gear; covered in a later phase
- **Watermark and Reconciliation Metadata**: Per-caller-gear and per-tenant ingestion metadata (watermarks, event counts, latest event timestamps, ingestion statistics) and the corresponding metadata-exposure API are out of scope for phase 1 of the gear; covered in a later phase. External reconciliation workflows that depend on this metadata are out of scope for the gear entirely

## 5. Functional Requirements

### 5.1 Usage Ingestion

#### Usage Record Ingestion

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-ingestion`

The system **MUST** accept usage records from authenticated usage sources. Each usage record represents a single measurement of resource consumption attributed to a tenant.

- **Rationale**: Centralizing usage ingestion ensures all downstream consumers operate on the same data.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

#### Idempotent Ingestion

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-idempotency`

The system **MUST** require a client-provided idempotency key on every usage record. The system **MUST** reject any record submitted without an idempotency key with an actionable error. When a record is submitted whose idempotency key matches a previously accepted record for the same tenant and UsageType **and** every caller-supplied field is identical — value, timestamp, resource (resource_ref), subject (subject_ref), and metadata — the system **MUST** silently deduplicate the submission (no error, no duplicate record); this is the exact-equality retry case. When the key matches a previously accepted record for the same tenant and UsageType but **any** caller-supplied field differs from the stored record — including a metadata-only difference — the system **MUST** reject the submission with an actionable conflict error and **MUST NOT** silently drop the second write. The dedup boundary is per-tenant per-UsageType: the same idempotency key may legitimately reappear under a different tenant or a different UsageType without being treated as a duplicate. The idempotency window is **UNBOUNDED**: a key has no time-to-live, never expires, and is never intentionally reusable, so the per-tenant per-UsageType uniqueness of an idempotency key is permanent.

- **Rationale**: Client-side retries on transient failures can produce duplicate submissions; deduplication prevents incorrect aggregations. For counter UsageTypes, a retry of a keyless delta inflates the accumulated total without any means of detection or correction. For gauge UsageTypes, duplicate readings can still poison downstream consumers that derive counts, distinct timestamps, or rate-of-change signals from raw records. Requiring an idempotency key on every emission eliminates this data integrity risk at the calling gear, removes a semantics-dependent special case from the ingestion contract, and lets calling gears adopt a single retry pattern across all UsageTypes they emit. Splitting the same-key outcome is deliberate: an exact-equality retry is the benign at-least-once case and remains safe to absorb silently, but a key reused with different content is a caller bug. Surfacing that divergence as a conflict rather than silently dropping the second write protects billing-correctness and other downstream consumers from data that would otherwise be lost without any signal, while an unbounded window guarantees a key can never be silently recycled into a different record.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

#### Per-Record Extensible Metadata

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-fr-record-metadata`

The system **MUST** support a closed-shape metadata model declared per UsageType: every metadata key supplied on a usage record **MUST** be a member of the referenced UsageType's declared metadata-key list. Undeclared metadata keys **MUST NOT** be accepted — the system rejects such records at the gateway with an actionable validation error. All metadata values are treated as strings on the wire and at rest. The system **MUST** enforce a configurable maximum metadata size and **MUST** reject records exceeding the configured limit with an actionable error.

The metadata surface is **closed**: there is no free-form remainder, no open-extras escape hatch, and no silently-preserved undeclared properties. Downstream consumers (billing, reporting, analytics) extract declared keys by name; the Usage Collector's query surface addresses the same declared keys.

- **Rationale**: Different usage sources need to attach context-specific properties to usage records (e.g., LLM model name, token type, request category, geographic region) that enable downstream reporting and analytics. A closed-shape model lets UsageType authors declare exactly the keys that matter, gives downstream consumers a stable contract they can address by name, and removes the open-extras attack surface (undeclared keys can no longer be smuggled into the store and silently preserved). String-only value typing keeps the gateway validation cheap — a declared-keys membership check — and aligns the v1 surface with the quota-reporting downstream consumer narrowing.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`, `cpt-cf-usage-collector-actor-platform-developer`

### 5.2 UsageType Semantics

#### Counter UsageType

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-counter-semantics`

The system **MUST** enforce counter semantics: calling gears submit non-negative delta values representing consumption since their last report. The system **MUST** reject records for counter UsageTypes with negative values. The system **MUST** accumulate submitted deltas into a persistent, signed-net cumulative `SUM` per (tenant, usage_type) tuple, which `cpt-cf-usage-collector-fr-usage-compensation` MAY reduce via append-only compensation entries.

- **Rationale**: Delta-based reporting decouples the calling gear's internal state from the Usage Collector's persistent totals. Calling gears never report cumulative values, so process restarts and counter resets in the calling gear are transparent — a restart simply results in the next emission starting from zero again, which is valid.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

#### Gauge UsageType

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-gauge-semantics`

The system **MUST** support gauge UsageTypes representing point-in-time values. Records for gauge UsageTypes **MUST** be stored as-is without monotonicity constraints or delta accumulation.

- **Rationale**: Gauges represent instantaneous measurements (e.g., current active connections, memory usage in bytes) that naturally fluctuate and have no meaningful cumulative total.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

### 5.3 Attribution & Isolation

#### Tenant Attribution

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-tenant-attribution`

1. The system **MUST** attribute every usage record to a tenant supplied by the caller in the request.
2. The system **MUST** authorize the caller's tenant attribution via the platform PDP before any record is accepted, verifying that the authenticated caller is permitted to emit records for the specified tenant. This covers both same-tenant emission and parent→subtenant scenarios (e.g., a platform-level metering agent collecting usage for resources owned by its subtenants).
3. The gateway **MUST** independently validate tenant attribution on ingest as a defense-in-depth check.

- **Rationale**: Requiring callers to supply the target tenant explicitly supports all emission scenarios — including remote forwarders and external systems that emit records on behalf of multiple tenants — through a single uniform path. PDP authorization remains the security boundary enforcing which tenants a given caller is permitted to report for.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

#### Resource Attribution

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-resource-attribution`

Every usage record **MUST** be attributed to a specific resource instance within a tenant, identified by a resource ID and resource type. Resource attribution is mandatory; the system **MUST** reject records that omit either field.

- **Rationale**: Per-resource attribution enables granular billing, per-resource quota enforcement, and detailed usage analysis at the resource level. Mandatory attribution ensures downstream consumers always have a resource scope to aggregate and filter on, without needing to handle the absence of this field.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

#### Subject Attribution

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-subject-attribution`

1. The system **MUST** support attributing usage records to a subject (user, service account, or other principal) within a tenant, identified by a caller-supplied subject ID and, when available, an optional subject type. Subject attribution is optional per usage record to accommodate system-level resource consumption not attributable to a specific subject (e.g., background jobs where per-user attribution is not meaningful); when subject attribution is supplied, the subject ID **MUST** be present, the subject type **MAY** be omitted for systems without subject-type taxonomies, and a subject type **MUST NOT** be supplied without a subject ID.
2. When a subject is supplied, the system **MUST** authorize the caller's subject attribution via the platform PDP before any record is accepted, verifying that the authenticated caller is permitted to emit records attributed to the specified subject ID and, when supplied, subject type. When no subject ID is supplied, PDP subject validation is skipped.
3. The system **MUST NOT** derive subject identity from the caller's SecurityContext: subject attribution is always caller-supplied, never implicitly populated from the authenticated principal.

- **Rationale**: Per-subject attribution enables chargeback, per-subject quota enforcement, and visibility into which principals drive consumption within a tenant. Accepting the target subject explicitly from the caller — rather than implicitly from the caller's own SecurityContext — supports emission scenarios where the calling service attributes consumption to subjects other than itself (e.g., a service emitting per-user records on behalf of the users it serves, or a remote forwarder relaying records originally produced by multiple named subjects). PDP authorization remains the security boundary enforcing which subjects a given caller is permitted to report for, preventing spoofing.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`
- **Data Classification**: Subject IDs are opaque platform identifiers; PII handling is owned by the platform identity layer (see [§6.2](#62-nfr-exclusions) NFR Exclusions).

#### Tenant Isolation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-tenant-isolation`

The system **MUST NOT** grant any caller access to a tenant's usage data — for reads or writes — without an explicit PDP authorization for that tenant. The system **MUST** treat every tenant scope independently: no caller is implicitly authorized for any tenant, and authorization for one tenant **MUST NOT** be inferred from authorization for another (sibling, parent, or child). Cross-tenant access is permitted only when the PDP explicitly authorizes the authenticated caller for the target tenant (e.g., a parent tenant administrator authorized to read its subtenants' usage). The system **MUST** fail closed on authorization failures.

- **Rationale**: Tenant data isolation is a security and compliance requirement, but parent→subtenant hierarchies and platform-level administrative roles legitimately require cross-tenant visibility. Anchoring isolation on PDP authorization keeps the security boundary precise while supporting the hierarchical scenarios the platform exposes (see `cpt-cf-usage-collector-fr-tenant-attribution`, `cpt-cf-usage-collector-fr-ingestion-authorization`).
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`, `cpt-cf-usage-collector-actor-usage-consumer`

#### Ingestion Authorization

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-ingestion-authorization`

1. The system **MUST** authorize each usage record emission before it is persisted. The security boundary for ingestion authorization is the PDP check on the caller's authenticated identity (including the calling-gear identity carried in the platform-resolved `SecurityContext`) against the supplied tenant, resource, and referenced UsageType.
2. The system **MUST** verify the caller is permitted to emit records attributed to the specified tenant and resource, against the calling-gear identity from `SecurityContext` and the referenced UsageType, before any record is accepted.
3. The system **MUST** validate that the referenced UsageType is registered, rejecting records that reference an unknown UsageType.
4. Authorization failures **MUST** be surfaced immediately to the caller before any domain operation is committed.
5. The system **MUST** fail closed: unauthorized records are never persisted, and there is no silent discard of denied emissions.

- **Rationale**: Anchoring authorization on the authenticated caller (with the calling-gear identity derived from `SecurityContext`) plus the caller-supplied attribution tuple (tenant, resource, UsageType) lets the PDP enforce per-caller emission scope without trusting any caller-supplied claim of "who is emitting". UsageType existence validation preserves data quality by ensuring records reference known UsageTypes.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

### 5.4 Pluggable Storage

#### Pluggable Storage Backend

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-pluggable-storage`

The system **MUST** support pluggable storage backends. Operators **MUST** be able to select the active backend without changing Usage Collector product behavior.

**Scope**: Pluggable storage covers both **usage records** (ingestion, query, deactivation, compensation) and the **usage-type catalog**. The usage-type catalog is the sole catalog and is reached through the storage plugin; details in DESIGN.

- **Rationale**: Pluggable storage avoids lock-in and allows operators to choose the backend that fits their needs. Co-locating catalog rows and usage rows on the same plugin-owned backend lets the deletion path enforce referential integrity natively instead of relying on cross-store coordination, and keeps the storage plugin the single seam through which the gear reaches durable state.
- **Actors**: `cpt-cf-usage-collector-actor-platform-operator`, `cpt-cf-usage-collector-actor-storage-backend`

### 5.5 Usage Query & Aggregation

#### Aggregated Usage Query

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-query-aggregation`

The system **MUST** provide an API for querying aggregated usage data. Queries **MUST** support time-bounded aggregation for exactly one UsageType and **SHOULD** allow consumers to narrow and group results by tenant, subject, resource, and time period where authorized. The supported aggregation operations and wire-level filters are defined in DESIGN.md and the OpenAPI contract.

The system **MUST** reject aggregation requests that omit the usage_type filter or supply more than one usage_type value, with an actionable error.

The system **MUST** authorize each query via the platform PDP. PDP-returned constraints define the authorization boundary and **MUST** be applied as query filters before execution. User-supplied filters (including `tenant`) **MUST** be applied in addition to PDP-returned constraints — they can only further narrow the result set, never widen it beyond the PDP-authorized scope. The system **MUST** fail closed on authorization failures (PDP denial or empty constraints).

- **Rationale**: Downstream consumers (billing, dashboards) need aggregated views without fetching and processing raw records. Restricting each aggregation to a single UsageType ensures the aggregated values share consistent semantics and units — combining counts, byte volumes, or duration measures across different UsageTypes is meaningless and would mask data-quality issues. Product-level filtering and grouping still enable rich breakdowns within a UsageType while preserving PDP-authorized scope.
- **Actors**: `cpt-cf-usage-collector-actor-usage-consumer`, `cpt-cf-usage-collector-actor-tenant-admin`

#### Raw Usage Query

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-fr-query-raw`

The system **MUST** provide an API for querying raw usage records as paged results. Queries **MUST** support a mandatory time range and **SHOULD** allow consumers to narrow results by tenant, UsageType, subject, and resource where authorized. Paging mechanics and wire-level filter details are defined in DESIGN.md and the OpenAPI contract.

The system **MUST** authorize each query via the platform PDP using the same decision and constraint-enforcement model as the aggregation query path: PDP-returned constraints define the authorization boundary, and user-supplied filters (including `tenant`) only further narrow the result set within that scope. The system **MUST** fail closed on authorization failures.

- **Rationale**: Some consumers need access to individual records for auditing, debugging, or dispute resolution.
- **Actors**: `cpt-cf-usage-collector-actor-usage-consumer`, `cpt-cf-usage-collector-actor-tenant-admin`

### 5.6 Corrections (Event Deactivation & Usage Compensation)

The Usage Collector exposes two complementary correction primitives: **event deactivation** is cross-kind whole-row error retraction (any entry, operator-only, one-way `active → inactive` latch), and **usage compensation** is counter-only append-only value-reversal (caller-emitted on the ingestion path, with a strictly-negative value referencing the original entry). The two are disjoint by purpose and aggregation contract: deactivation removes a row from every aggregation; compensation reduces the netted `SUM` only and leaves `COUNT` / `MIN` / `MAX` / `AVG` untouched.

#### Individual Event Deactivation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-event-deactivation`

The system **MUST** support deactivating individual usage events by transitioning the event's `status` from active to `inactive` while retaining the event for downstream reference. Deactivation **MUST NOT** modify any property of the record other than `status`. Downstream consumers **MUST** be able to distinguish active from inactive records when querying, and inactive records **MUST** remain queryable.

Deactivation is one-way: the Usage Collector does not provide a reactivation operation. The system **MUST** reject deactivation requests targeting an already-inactive record with an actionable error.

Deactivation applies uniformly to any entry — both usage rows and compensation rows can be deactivated through the same operation. Deactivation of a usage row with one or more active compensations referencing it triggers a **depth-1 cascade** to those compensations, flipping them to `inactive` in the same one-way step, so the net `SUM` returns to the state it held before either the usage record or its compensations were accepted. The cascade is strictly depth-1 by construction (a compensation row cannot itself be compensated; see `cpt-cf-usage-collector-fr-usage-compensation`).

- **Rationale**: Deactivation retires a record from downstream consumption without losing its history, letting storage plugins, query consumers, and aggregation pipelines reason about active/inactive transitions as a first-class lifecycle event. Making deactivation one-way keeps each record's lifecycle monotonic. Cascading to active compensations preserves the post-correction `SUM` invariant without forcing a second operator action.
- **Actors**: `cpt-cf-usage-collector-actor-platform-operator`

#### Usage Compensation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-usage-compensation`

The system **MUST** accept counter-only, append-only **compensation** entries that partially reverse a previously reported usage value through `SUM` netting, without rewriting or deactivating the original row. A compensation entry is submitted via the **same ingestion path** used for usage records (no dedicated compensate endpoint, SDK method, or storage-plugin call exists), is attributed via the platform PDP on the caller's identity, and is protected by the existing mandatory idempotency key (cross-reference `cpt-cf-usage-collector-fr-idempotency`).

The system **MUST** enforce the following invariants at ingestion before persistence:

- **Counter-only**: a compensation entry referencing a `gauge` UsageType **MUST** be rejected with an actionable error. Compensation is defined only for `counter` UsageTypes; the only correction available for a `gauge` UsageType is deactivation (cross-reference `cpt-cf-usage-collector-fr-event-deactivation`).
- **Strictly negative value**: a compensation entry on a `counter` UsageType **MUST** carry a value strictly less than zero; zero and positive values are rejected with an actionable error.
- **Valid reference to the original entry (ingestion-time)**: every compensation entry **MUST** reference an existing usage entry that shares its tenant and UsageType and is currently `active`. Any failure is rejected with an actionable error. The "must be active" check is the concurrency boundary: a compensation referencing a row that is concurrently being deactivated is rejected by this check, without distributed coordination.
- **Aggregation effect**: a compensation entry **MUST** affect `SUM` only — `SUM(value)` over `active` rows nets usage and compensation signed values. `COUNT`, `MIN`, `MAX`, and `AVG` **MUST** operate over usage entries only ("compensation entries adjust SUM; they are not events").
- **Cascade on deactivation**: when a usage row that has one or more active compensations referencing it is deactivated, the system **MUST** apply the depth-1 cascade defined in `cpt-cf-usage-collector-fr-event-deactivation`, flipping the referencing compensation rows to `inactive` in the same one-way step.

The system **MUST NOT** support compensating a compensation row. The system **MUST NOT** validate non-negative net `SUM` and **MUST NOT** emit negative-net detection, alerts, or downstream reconciliation; per-record outstanding balances and lot / FIFO-LIFO tracking are explicit non-goals.

- **Rationale**: Counter value-reversal is a distinct concern from whole-row retraction. Compensation supports partial give-backs (capacity refunds, partial revocations) without rewriting the original usage row, preserving the append-only invariant and the audit history. Routing compensation through the same ingestion path as usage reuses the existing PDP attribution and idempotency machinery, keeps the public contract surface stable, and yields netting deterministically through `SUM` without any business-logic computation inside the metering substrate.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`
- **Depends on**: `cpt-cf-usage-collector-fr-ingestion`, `cpt-cf-usage-collector-fr-idempotency`, `cpt-cf-usage-collector-fr-counter-semantics`, `cpt-cf-usage-collector-fr-event-deactivation`, `cpt-cf-usage-collector-fr-ingestion-authorization`

### 5.7 Usage Types

UsageTypes are platform-global definitions: a UsageType exists once for the whole deployment and is referenced by any tenant's usage records. UsageTypes are not scoped to or owned by tenants.

#### UsageType Existence and Semantics Enforcement

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`

The system **MUST** reject any usage record that references an unregistered UsageType. The system **MUST** enforce semantics-dependent invariants based on the referenced UsageType's semantics — in particular, records for counter UsageTypes with negative delta values **MUST** be rejected (cross-reference [§5.2](#52-usage-type-semantics) `cpt-cf-usage-collector-fr-counter-semantics`). The counter/gauge semantics is derived from the registered UsageTypeGtsId prefix. Rejections **MUST** be returned to the caller immediately with an actionable error before any record is accepted for delivery.

A UsageType is identified by a UsageTypeGtsId and described by its derived counter/gauge semantics and an optional unit label. Beyond the semantics-dependent invariants and the closed metadata-key list declared on the UsageType at registration (see `cpt-cf-usage-collector-fr-usage-type-registration`), the Usage Collector **MUST NOT** require per-record schemas beyond the UsageType's declared metadata keys.

- **Rationale**: Restricting validation to existence and semantics invariants keeps UsageType registration lightweight while preserving the data-integrity guarantees that matter: records that reference unknown UsageTypes cannot enter the store, and counter accumulation cannot be poisoned by negative deltas. Per-record metadata ([§5.1](#51-usage-ingestion) `cpt-cf-usage-collector-fr-record-metadata`) is closed-shape: every metadata key on a usage record **MUST** be a member of the UsageType's declared keys, undeclared keys are rejected at the gateway, and all values are treated as strings end-to-end. Deriving semantics from the UsageTypeGtsId prefix collapses two registration-time invariants into one and removes a class of "semantics disagrees with identifier" inconsistency bugs.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`

#### UsageType Registration

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-usage-type-registration`

The system **MUST** allow platform operators to register a new UsageType via API without code changes or service redeployment. A registration request specifies the UsageTypeGtsId and the closed metadata-key list — an array of strings naming every metadata key the UsageType will accept on usage records (per [§5.1](#51-usage-ingestion) `cpt-cf-usage-collector-fr-record-metadata`). Counter/gauge semantics is derived from the UsageTypeGtsId prefix; at register time the system **MUST** validate the identifier and the derived semantics, and **MUST** reject malformed or unknown-prefix identifiers with an actionable validation error. The derived semantics governs the invariants in `cpt-cf-usage-collector-fr-counter-semantics` / `cpt-cf-usage-collector-fr-gauge-semantics`. Registered UsageTypes become immediately available for ingestion across all tenants.

Primary use cases: AI/LLM token metering (input/output tokens, custom credit units), compute metering (vCPU-hours, GPU-hours), API request metering (calls by tenant and endpoint), storage metering (GB-hours across tiers), and network transfer (bytes ingress/egress).

The UsageTypeGtsId **MUST** be unique across the deployment; duplicate registration requests **MUST** be rejected with an actionable error. Registration **MUST** be authorized by the platform PDP against the caller's identity; unauthorized requests are rejected before any change is made.

When a UsageType is registered, the platform operator **MUST** also configure the PDP authorization policies that declare which calling-gear identities are permitted to emit records referencing this UsageType, and for which tenants. The Usage Collector does not store this authorization mapping internally — it is owned by the PDP, which reads the calling-gear identity from the platform-resolved `SecurityContext` at emit time.

Registration is available on both the REST and in-process SDK surfaces; surface details are in DESIGN.

- **Rationale**: New resource types (AI tokens, GPU-hours, custom credit units) must be meterable without service redeployment. Declaring a closed metadata-key list on the UsageType lets the gateway validate declared per-record keys at ingest with a cheap membership check while giving downstream consumers a stable, addressable contract — and removes the open-extras attack surface (undeclared keys are validation errors rather than silently-preserved extras). Deriving counter/gauge semantics from the UsageTypeGtsId prefix collapses semantics and identifier into a single invariant, eliminating a class of "semantics disagrees with identifier" registration bugs. Pushing caller-gear-to-UsageType authorization into PDP avoids duplicating policy data; exposing the same operation on the SDK in addition to REST lets in-process callers register UsageTypes without round-tripping the REST surface.
- **Actors**: `cpt-cf-usage-collector-actor-platform-operator`

#### UsageType Deletion

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-usage-type-deletion`

The system **MUST** allow platform operators to delete a registered UsageType via API. Deletion **MUST** be authorized by the platform PDP against the caller's identity.

Deletion is **referential**: the system **MUST** reject deletion of a UsageType that is referenced by any existing usage row, returning a deterministic, structured "usage_type referenced" error to the caller. Referential delete protection on the usage-type catalog is enforced by the storage plugin so the rejection is atomic with the delete attempt and does not depend on cross-store coordination (mechanics in DESIGN).

After a successful (i.e., unreferenced) deletion, the UsageTypeGtsId becomes available for re-registration. Any subsequent ingestion attempt referencing the deleted UsageType is rejected by `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics` until the UsageType is re-registered.

Deletion is available on both the REST and in-process SDK surfaces; surface details are in DESIGN.

- **Rationale**: Referential delete eliminates the orphaned-attribution failure mode that an unconditional delete leaves behind, and enforcing it natively at the storage layer (rather than an application-level guard) makes the constraint atomic with the delete and survives any future caller bypassing the gateway. Exposing the operation on the SDK in addition to REST keeps the two surfaces convergent on a single domain service over the plugin-owned catalog.
- **Actors**: `cpt-cf-usage-collector-actor-platform-operator`

### 5.8 Data Classification

#### Data Classification

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-fr-data-classification`

The system **MUST** treat its persisted data as one of three classes:

- **Opaque platform identifiers** (tenant ID, subject ID, resource ID, UsageTypeGtsId) — internal platform references issued upstream. The Usage Collector **MUST NOT** interpret, decode, or correlate these identifiers to natural persons; PII management belongs to the platform identity layer.
- **Operational telemetry** (usage record value, timestamp, idempotency key, deactivation status) — non-personal metering data.
- **Caller-supplied metadata** (the optional per-record metadata object) — opaque to the Usage Collector. Calling gears **MUST NOT** place PII, payment data, regulated health data, or credentials into metadata; this is a product-level contract on usage sources, reiterated to integrators in the API documentation.

- **Rationale**: Explicit classification bounds the data the gear holds and keeps Privacy by Design, regulatory, and residency obligations delegated to the platform layer and the operator-selected plugin.
- **Actors**: `cpt-cf-usage-collector-actor-usage-source`, `cpt-cf-usage-collector-actor-platform-developer`, `cpt-cf-usage-collector-actor-platform-operator`

## 6. Non-Functional Requirements

### 6.1 Gear-Specific NFRs

#### Query Latency

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-query-latency`

Aggregation queries over a 30-day range for a single tenant **MUST** complete within 500ms at p95 under the load envelope defined by `cpt-cf-usage-collector-nfr-throughput-profile` (sustained ≥ 10,000 records/sec ingestion, ≥ 100 concurrent aggregation queries, no active burst in progress), measured over a ≥ 30-minute steady-state window.

- **Threshold**: p95 ≤ 500ms over a ≥ 30-minute steady-state window inside the `cpt-cf-usage-collector-nfr-throughput-profile` envelope; permitted measurement tolerance ±10% (i.e., p95 ≤ 550ms accepted for any single steady-state window) provided the 30-minute trailing trend stays at or below 500ms.
- **Rationale**: Interactive dashboard and billing queries need timely responses. Anchoring on the throughput profile and a measurement tolerance removes the ambiguity in the prior wording and makes the criterion repeatable.
- **Architecture Allocation**: See DESIGN.md

#### High Availability

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-availability`

The system **MUST** maintain 99.95% monthly availability for usage ingestion endpoints.

- **Threshold**: 99.95% uptime per calendar month
- **Rationale**: Usage collection is on the critical path for all billable operations.
- **Architecture Allocation**: See DESIGN.md

#### Ingestion Throughput

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-throughput`

The system **MUST** sustain ingestion of at least 10,000 usage records per second under the steady-state load envelope defined by `cpt-cf-usage-collector-nfr-throughput-profile` (sustained ≥ 10,000 records/sec; concurrent aggregation queries ≤ 100; no active burst in progress; measurement window ≥ 30 minutes of steady-state operation; sample-mean and p95 reported separately).

- **Threshold**: ≥ 10,000 records/sec sustained sample-mean over a ≥ 30-minute steady-state measurement window; instantaneous 1-minute sample-mean tolerance ≥ 0.95 × sustained rate (i.e., ≥ 9,500 records/sec for any 1-minute sample inside the steady-state window).
- **Rationale**: High-volume services (LLM Gateway, API Gateway) generate significant event throughput; the ingestion path must not become a bottleneck. Anchoring on the throughput profile removes the ambiguity in "normal operation" by pinning the test condition to the sustained, burst, and concurrent-query envelope defined in `cpt-cf-usage-collector-nfr-throughput-profile`.
- **Architecture Allocation**: See DESIGN.md

#### Ingestion Latency

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-ingestion-latency`

The system **MUST** complete usage record ingestion within 200ms at p95 under the load envelope defined by `cpt-cf-usage-collector-nfr-throughput-profile` (sustained ≥ 10,000 records/sec, burst ≤ 30,000 records/sec for ≤ 5 minutes per 60-minute window, ≥ 100 concurrent aggregation queries, ≥ 700,000,000 accepted calls per 24-hour day), measured at the platform gateway over a ≥ 30-minute steady-state window.

- **Threshold**: p95 ≤ 200ms over a ≥ 30-minute steady-state measurement window inside the `cpt-cf-usage-collector-nfr-throughput-profile` envelope; permitted measurement tolerance ±10% (i.e., p95 ≤ 220ms accepted for any single steady-state window) provided the 30-minute trailing trend stays at or below 200ms.
- **Rationale**: Low ingestion latency prevents blocking in usage source services. Anchoring on the throughput profile and a measurement tolerance removes the ambiguity in "normal load" and makes the criterion repeatable.
- **Architecture Allocation**: See DESIGN.md

#### Workload Isolation

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-nfr-workload-isolation`

The system **MUST** ensure that aggregation query workloads do not degrade ingestion latency. These workloads **MUST** be isolated from the ingestion path such that concurrent execution maintains ingestion p95 latency within the `cpt-cf-usage-collector-nfr-ingestion-latency` threshold.

- **Threshold**: Ingestion p95 latency remains ≤ 200ms during concurrent query operations
- **Rationale**: Aggregation queries are analytical workloads that can compete for storage resources with the latency-sensitive ingestion path.
- **Architecture Allocation**: See DESIGN.md

#### Query Freshness

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-query-freshness`

The system **MUST** publish a plugin-agnostic consistency contract between the synchronous ingestion ack path and the subsequent raw / aggregated / catalog query surfaces. The contract is **floor-and-ceiling**: the gear floor is the minimum every active plugin honours under default deployment posture, and each plugin's deployment guide MAY advertise a stronger ceiling.

- **Floor (gear-level)**: ingestion `Acknowledged` is durable and the `(tenant_id, gts_id, idempotency_key)` dedup tuple is permanently visible to subsequent ingestion attempts. Visibility of the same record through `cpt-cf-usage-collector-fr-query-raw`, `cpt-cf-usage-collector-fr-query-aggregation`, and the catalog read paths reached by `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics` is **eventually consistent with no upper bound** relative to the ingestion ack. The floor is per-`(tenant_id, gts_id)`; no cross-tenant or cross-usage_type ordering claim is made. No monotonic-reads-per-`(tenant_id, gts_id)` guarantee at the floor.
- **Ceiling (per-plugin)**: each `usage-collector-plugin-<backend>` deployment guide **MUST** publish the plugin's actual consistency profile (e.g., "sync, single-node", "bounded-staleness ≤ N ms", "eventual, no bound — see workload-isolation routing"). Consumers that depend on a tighter bound consciously couple themselves to that plugin's ceiling; the coupling **MUST** be recorded in the consumer's own design document.
- **Consumer rule**: read-after-write calling-gear flows (admission control, post-emit summary, immediate-readback dashboards) **MUST NOT** be designed against the query surfaces. Same-request outcome flows **MUST** consume the ingestion ack. Near-real-time observers poll within `cpt-cf-usage-collector-nfr-query-latency` and accept lag bounded by the active plugin's published ceiling.
- **Threshold**: Floor: no gear-level numeric bound (absence claim, verified by documentation review over DESIGN §3.10, `plugin-spi.md` §"Consistency profile", and the feature pointers). Ceiling: per-plugin published profile, verified against each plugin's release-readiness review.
- **Rationale**: The workload-isolation NFR routes ingestion and query to isolated backend pools (`cpt-cf-usage-collector-nfr-workload-isolation`); that isolation creates queryability lag between the ack path and the query path that nothing else names. Publishing the floor at PRD level lets consumers code defensively against the weakest plugin without reading per-plugin documentation, and lets plugin authors advertise stronger ceilings honestly rather than under an implicit gear-wide claim that overpromises for backends like ClickHouse-replicated. The architectural decision is recorded in DESIGN §5.1 (consistency-contract ADR).
- **Architecture Allocation**: See DESIGN.md §3.10 (Consistency Contract).

#### Plugin Contract Stability Across Versions

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-plugin-contract-stability`

The Plugin SPI (`cpt-cf-usage-collector-interface-plugin`), the SDK trait (`cpt-cf-usage-collector-interface-sdk-client`), and the REST API (`cpt-cf-usage-collector-interface-rest-api`) **MUST** each remain stable within a major version. A plugin built against Plugin SPI version `N` **MUST** continue to work against version `N.x` for any value of `x`; the same guarantee applies to in-process consumers of the SDK trait and to remote consumers of the REST API. Breaking changes **MUST** be expressed as a new major version that coexists with the prior major version for at least one migration window, so plugin authors, consumer gears, and remote callers can migrate on independent schedules from the Usage Collector itself.

- **Threshold**: Each public surface compiled or wired against the initial released major version **MUST** continue to function unchanged against every minor and patch release of the same major version; at most one prior major version is supported concurrently per surface.
- **Rationale**: Plugin authors, downstream consumer gears, and remote usage sources are typically not the same teams as Usage Collector maintainers (e.g., a TimescaleDB or ClickHouse plugin maintained by an external storage team, or a billing system in a separate release train). Forcing them to recompile or redeploy on every minor Usage Collector release creates ecosystem coordination overhead and discourages reuse.
- **Architecture Allocation**: See DESIGN.md

#### Throughput Profile

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-nfr-throughput-profile`

The system **MUST** sustain the following ingestion and query workload profile at launch capacity:

- **Sustained ingestion**: ≥ 10,000 usage records per second (cross-reference `cpt-cf-usage-collector-nfr-throughput`).
- **Peak ingestion burst**: ≥ 30,000 usage records per second for ≤ 5 minutes in any 60-minute window without breaching `cpt-cf-usage-collector-nfr-ingestion-latency` (p95 ≤ 200ms).
- **Concurrent query consumers**: ≥ 100 active aggregation queries without breaching `cpt-cf-usage-collector-nfr-query-latency` (p95 ≤ 500ms) or degrading ingestion p95 (`cpt-cf-usage-collector-nfr-workload-isolation`).
- **Daily transaction volume**: ≥ 700,000,000 accepted ingestion calls per 24-hour day at the sustained rate.
- **Seasonal / cyclical pattern**: monthly billing-cycle close is the highest concurrent-query period; ingestion volume is not expected to spike seasonally beyond the burst envelope.

- **Threshold**: Sustained ≥ 10,000 records/sec; burst ≥ 30,000 records/sec for ≤ 5 minutes per 60-minute window; ≥ 100 concurrent aggregation queries; ≥ 700,000,000 accepted ingestion calls per 24-hour day.
- **Rationale**: Documenting the steady-state, peak, burst, and concurrent-consumer profile lets capacity planning, alert thresholds, and load tests share one product-level envelope.
- **Architecture Allocation**: See DESIGN.md

#### Operational Visibility

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-nfr-operational-visibility`

Usage Collector domain metrics **MUST** be integrated into shared platform dashboards and alert routing. At minimum, operator treatment **MUST** exist for ingestion latency, ingestion error rate, query latency, PDP error rate, and storage-plugin readiness. Every accepted and rejected API operation **MUST** emit a structured log record carrying the `correlation_id` propagated unchanged from the inbound platform-resolved `SecurityContext` so gear activity reconciles with platform gateway access logs.

### 6.2 NFR Exclusions

The following commonly applicable NFR categories are not applicable to this gear:

- **Safety (ISO/IEC 25010:2023 §4.2.9)**: Not applicable — the Usage Collector is a server-side data API with no physical interaction, no safety-critical operations, and no ability to cause harm to people, property, or the environment.
- **End-user UI accessibility and usability**: Not applicable — the Usage Collector exposes no user-facing UI. Developer, API consumer, and operator experience is delivered through the SDK trait, REST API, and platform-level documentation and support channels.
- **Internationalization / Localization**: Not applicable — the gear exposes no user-facing text, labels, or locale-sensitive output.
- **Privacy by Design (GDPR Art. 25) as a standalone regulatory conformance claim**: Not applicable. Subject IDs stored by the Usage Collector are opaque internal platform identifiers; PII management is the responsibility of the platform identity layer (cross-reference [§5.3](#53-attribution-isolation) Subject Attribution). Standalone GDPR Article 25 conformance is governed at platform level.
- **Regulatory Compliance (GDPR, HIPAA, PCI DSS, SOX) as standalone gear obligations**: Not applicable — this is an internal platform infrastructure gear. The gear handles no payment card data (PCI DSS N/A), no healthcare records (HIPAA N/A), and no financial-reporting source data (SOX N/A). Platform-level regulatory obligations are governed at the platform level.
- **Consent Management and Data Subject Rights (DSR) workflows**: Not applicable at gear level. Consent capture, withdrawal, and data-subject-rights execution (access, rectification, erasure, restriction, portability, objection) are owned by the platform identity, legal, and governance layers; the Usage Collector does not host a gear-local consent store or DSR workflow.
- **Data Sovereignty and Cross-Border Transfer policy at gear level**: Not applicable. Data residency, cross-border transfer restrictions, and replication topology are governed by the platform deployment topology and the operator-selected storage plugin's deployment profile (cross-reference [§4.2](#42-out-of-scope) deferred Multi-Region Replication).
- **Gear-Specific Disaster Recovery**: Not applicable as a standalone gear requirement. Recovery Point Objective (RPO), Recovery Time Objective (RTO), backup, and restore posture are governed by the platform's general disaster-recovery posture and the operator-selected storage backend's own DR mechanisms; the Usage Collector does not define gear-specific recovery thresholds.
- **Device / Platform Requirements (UX-PRD-004)**: Not applicable — the Usage Collector is server-side platform infrastructure with no UI client. It is consumed exclusively via the in-process SDK trait (`cpt-cf-usage-collector-interface-sdk-client`), the Plugin SPI (`cpt-cf-usage-collector-interface-plugin`), and the REST API (`cpt-cf-usage-collector-interface-rest-api`); no browser, mobile, desktop, offline, or responsive-design surfaces exist, so per-device, per-platform, and offline-mode obligations do not apply at gear level.
- **Inclusivity Requirements (UX-PRD-005)**: Not applicable — the Usage Collector serves a narrow technical audience (platform developers, platform operators, tenant administrators, and downstream consumer services) through the in-process SDK, Plugin SPI, and REST API. The gear exposes no end-user UI surface, no per-subject profile view, and no human-targeted content, so cognitive-accessibility, diverse-user-population, and cultural-sensitivity obligations remain at the platform level rather than being asserted as standalone gear obligations.

## 7. Public Library Interfaces

### 7.1 Public API Surface

The Usage Collector exposes three public surfaces: an in-process SDK trait consumed by platform gears, a Plugin SPI implemented by storage extensions, and a REST API consumed by remote usage sources, operator tooling, and downstream consumers. The REST API is the full product surface for ingestion, query, event deactivation, UsageType lifecycle, and health visibility. The SDK trait is a narrower in-process consumer surface, while the Plugin SPI is the storage-extension surface. The entries below describe stable capability surfaces at PRD level; detailed signatures and wire contracts are defined in DESIGN.md and the linked contract documents.

#### Usage Collector SDK

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-interface-sdk-client`

**Actors**: `cpt-cf-usage-collector-actor-usage-source`, `cpt-cf-usage-collector-actor-platform-developer`, `cpt-cf-usage-collector-actor-usage-consumer`

<!-- cpt-cf-id-content -->

**Type**: In-process async client trait
**Stability**: stable (V1)
**Description**: In-process consumer surface covering ingestion of usage and compensation records (`cpt-cf-usage-collector-fr-ingestion`, `cpt-cf-usage-collector-fr-usage-compensation`, `cpt-cf-usage-collector-fr-idempotency`), raw query (`cpt-cf-usage-collector-fr-query-raw`), aggregated query (`cpt-cf-usage-collector-fr-query-aggregation`), and individual event deactivation (`cpt-cf-usage-collector-fr-event-deactivation`). Operator and UsageType-lifecycle operations are intentionally REST-only.
**Consumed / Provided Data**: consumes usage submissions, raw and aggregated query requests, and deactivation requests; provides acceptance acknowledgements, raw usage views, and aggregated usage results. Operator-only data classes are intentionally not exposed on this trait.
**Availability / Fallback**: in-process trait availability follows the Usage Collector gear and its active storage dependency. The SDK does not provide an alternate persistence path or synthesize usage data.
**Breaking Change Policy**: Major version bump required for trait method signature changes; within a version, only additive changes (new methods with default implementations). The platform supports one previous major version of this trait concurrently to give consumer gears a migration window, consistent with `cpt-cf-usage-collector-nfr-plugin-contract-stability`.
See DESIGN.md for the trait signature.

<!-- cpt-cf-id-content -->

#### Plugin SPI

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-interface-plugin`

**Actors**: `cpt-cf-usage-collector-actor-storage-backend`

<!-- cpt-cf-id-content -->

**Type**: Storage plugin SPI
**Stability**: stable (V1)
**Description**: Storage-extension surface implemented by each plugin for persistence of usage and compensation records (`cpt-cf-usage-collector-fr-pluggable-storage`, `cpt-cf-usage-collector-fr-usage-compensation`), raw and aggregated query (`cpt-cf-usage-collector-fr-query-raw`, `cpt-cf-usage-collector-fr-query-aggregation`), and individual event deactivation including the depth-1 cascade to active compensations referencing a deactivated usage record (`cpt-cf-usage-collector-fr-event-deactivation`). The operator selects the active backend via configuration (see `cpt-cf-usage-collector-fr-pluggable-storage`).
**Consumed / Provided Data**: consumes usage persistence, raw and aggregated query, and deactivation requests; provides persistence acknowledgements, raw usage views, and aggregated usage results.
**Availability / Fallback**: backend-bound — the SPI's availability tracks the selected storage backend. There is no parallel storage path in the Usage Collector.
**Breaking Change Policy**: Plugin contract versioned with the gear; breaking trait changes require a coordinated release with every plugin implementation. The platform supports one previous major version of the Plugin SPI concurrently to give plugin authors a migration window.
See DESIGN.md for the trait signature.

<!-- cpt-cf-id-content -->

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-interface-rest-api`

**Actors**: `cpt-cf-usage-collector-actor-usage-source`, `cpt-cf-usage-collector-actor-usage-consumer`, `cpt-cf-usage-collector-actor-platform-operator`, `cpt-cf-usage-collector-actor-tenant-admin`

<!-- cpt-cf-id-content -->

**Type**: HTTP REST API
**Stability**: stable (V1)
**Description**: HTTP API consumed by remote usage sources, operator tooling, and downstream consumers. This REST surface is the full product operation surface for the gear. Capability categories:

- Ingestion of usage and compensation records — `cpt-cf-usage-collector-fr-ingestion`, `cpt-cf-usage-collector-fr-usage-compensation`, `cpt-cf-usage-collector-fr-idempotency`.
- Raw query — `cpt-cf-usage-collector-fr-query-raw`
- Aggregated query — `cpt-cf-usage-collector-fr-query-aggregation`
- Individual event deactivation — `cpt-cf-usage-collector-fr-event-deactivation`
- UsageType registration and lifecycle (create, list, get, delete) — `cpt-cf-usage-collector-fr-usage-type-registration`, `cpt-cf-usage-collector-fr-usage-type-deletion`, `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`
- Health

The detailed wire contract is authored in `usage-collector-v1.yaml` (sibling to DESIGN.md) and the endpoint enumeration is in DESIGN §3.3 Endpoints Overview; the yaml is authoritative for wire schemas and the canonical error envelope shape. Per-endpoint stability for v1 is captured in the DESIGN §3.3 Endpoints Overview table; the major-version stability contract is declared in the yaml info description. Technical API details are intentionally not duplicated here.

**Consumed / Provided Data**: consumes usage submissions, raw and aggregated query requests, deactivation requests, UsageType lifecycle requests, and health requests; provides ingestion acknowledgements, raw usage views, aggregated usage results, UsageType catalog state, health visibility, and platform-standard errors.
**Availability / Fallback**: served behind the platform API gateway; authentication is performed by the platform gateway upstream of the collector, and PDP authorization is on the critical path. Read availability follows `cpt-cf-usage-collector-nfr-availability`.
**Breaking Change Policy**: Major version bump required (v1 → v2) for endpoint removal or incompatible request / response schema changes; within v1, only additive changes (new endpoints, new optional fields). The platform supports one previous major version of the REST API concurrently to give remote consumers a migration window, consistent with `cpt-cf-usage-collector-nfr-plugin-contract-stability`.
See DESIGN.md for endpoint contracts.

<!-- cpt-cf-id-content -->

### 7.2 External Integration Contracts

The Usage Collector requires two platform services as outbound dependencies — Platform PDP and platform registry/orchestration services for storage extension selection — and provides two outward contracts: a Storage Plugin Contract for storage extensions and a Downstream Usage Reader Contract for billing, quota enforcement, dashboards, and platform monitoring consumers. Caller authentication is performed by the ToolKit gateway upstream of the collector and is not an outbound dependency declared by this gear.

#### Platform PDP Contract

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-contract-authz-resolver`

<!-- cpt-cf-id-content -->

**Direction**: required from `authz-resolver`
**Protocol/Format**: Platform PDP authorization decisions for every ingestion, query, and operator-write operation.
**Consumed / Provided Data**: consumes caller identity and product-level operation context; receives permit/deny decisions and any authorized read-scope constraints.
**Availability / Fallback**: PDP authorization is on the critical path for every ingestion, query, and operator-write call; there is no fallback or cached-decision path. When the PDP is unreachable, all authorized operations fail closed (denied) with a deterministic platform-authorization error; the Usage Collector does not serve cached decisions or invent a permissive fallback.
**Compatibility**: Contract follows the platform authorization protocol; changes require coordinated release.

<!-- cpt-cf-id-content -->

#### Platform Registry / Orchestration Contract

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-contract-gts-registry`

<!-- cpt-cf-id-content -->

**Direction**: required from client
**Protocol/Format**: Platform registry and orchestration services support operator-selected storage extension resolution and lifecycle.
**Consumed / Provided Data**: consumes the operator-selected storage extension identity; receives the active storage extension needed for persistence and query capability.
**Availability / Fallback**: Storage extension resolution is required for gear readiness. When the required registry or orchestration dependency is unavailable during startup, the Usage Collector does not advertise readiness.
**Compatibility**: Selector identifiers follow the platform registry and orchestration protocols; changes require a coordinated release with the registry, the orchestrator, and every plugin implementation.

<!-- cpt-cf-id-content -->

#### Storage Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-contract-storage-plugin`

<!-- cpt-cf-id-content -->

**Direction**: provided by library (Plugin SPI offered to plugin authors implementing storage backends)
**Protocol/Format**: Storage Plugin SPI (`cpt-cf-usage-collector-interface-plugin`) implemented by storage backends selected by operators.
**Consumed / Provided Data**: the Usage Collector dispatches persistence, raw query, aggregated query, and individual deactivation requests; plugins return acknowledgements and usage results. Plugins **MUST NOT** invent records.
**Availability / Fallback**: A plugin's availability is its own concern; the Usage Collector treats plugin unavailability. There is no parallel local storage path in the Usage Collector.
**Compatibility**: The Plugin SPI follows `cpt-cf-usage-collector-nfr-plugin-contract-stability` — a plugin built against the initial released major version continues working against every minor and patch release of the same major version; breaking changes are expressed as a new major version that coexists with the prior major version during a migration window. Plugins ship on independent release schedules from the Usage Collector itself.

<!-- cpt-cf-id-content -->

#### Downstream Usage Reader Contract

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-contract-downstream-usage-reader`

<!-- cpt-cf-id-content -->

**Direction**: provided by library (read-only usage views consumed by downstream readers: billing, quota enforcement, dashboards, and platform monitoring)
**Protocol/Format**: Public REST API `cpt-cf-usage-collector-interface-rest-api` for out-of-process readers and, for in-process platform gears, the SDK trait `cpt-cf-usage-collector-interface-sdk-client`.
**Consumed / Provided Data**: downstream readers submit raw and aggregated query requests and health requests where applicable; the Usage Collector returns raw usage views, aggregated usage results, and health visibility. Business logic (pricing, rating, invoice generation, quota enforcement decisions) **MUST NOT** be performed inside the Usage Collector; it is the responsibility of the downstream reader.
**Availability / Fallback**: Query availability and latency follow `cpt-cf-usage-collector-nfr-query-latency` and `cpt-cf-usage-collector-nfr-availability`. PDP authorization is on the critical path and is fail-closed. Downstream readers **MUST NOT** invent usage state when the Usage Collector is unavailable.
**Compatibility**: Read shapes follow the Usage Collector's public versioning policy — at most one prior major version of the REST API and SDK trait is supported concurrently to give downstream readers a migration window. Additive changes within a major version do not break existing readers.

<!-- cpt-cf-id-content -->

### 7.3 Endpoints Summary

The canonical endpoint surface is defined in `usage-collector-v1.yaml` (sibling file) and mirrored in DESIGN §3.3 Endpoints Overview.

## 8. Use Cases

#### Emit Usage Records

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-usecase-emit`

**Actor**: `cpt-cf-usage-collector-actor-usage-source`

**Preconditions**:

- Actor is an authenticated usage source
- PDP authorization policies declare which UsageTypes the source is permitted to emit and for which tenants

**Main Flow**:

1. Usage source emits a usage record attributed to a tenant, resource, optional subject, and a registered UsageType
2. System authorizes the emission via PDP and validates the record against registered UsageType and semantics rules. Any failure is returned immediately to the caller before any record is accepted.
3. System accepts the record
4. Record becomes available for querying in the Usage Collector

**Postconditions**:

- Authorized, valid records are persisted in the storage backend and available for aggregation queries
- An exact-equality re-submission under an already-accepted idempotency key is silently deduplicated (no duplicate record); a same-key submission whose content differs is rejected with an actionable conflict error rather than silently dropped (cross-reference `cpt-cf-usage-collector-fr-idempotency`)

**Alternative Flows**:

- **Authorization denied**: System returns an error immediately; no record is accepted for delivery
- **Validation failed**: System returns an actionable error immediately; no record is accepted for delivery

#### Query Aggregated Usage

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-usecase-query-aggregated`

**Actor**: `cpt-cf-usage-collector-actor-usage-consumer`, `cpt-cf-usage-collector-actor-tenant-admin`

**Preconditions**:

- Actor is authenticated with a valid SecurityContext

**Main Flow**:

1. Consumer sends an aggregation query specifying a time range, UsageType, and desired grouping or rollup
2. System authorizes the query via PDP; PDP-returned constraints define the authorization boundary and user-supplied filters are applied in addition, only further narrowing the result set
3. System returns aggregated results scoped to the intersection of PDP-authorized scope and user-supplied filters

**Postconditions**:

- Consumer receives aggregated usage data within the intersection of PDP-authorized scope and user-supplied filters

**Alternative Flows**:

- **No data in range or scope**: System returns empty result set (not an error)
- **PDP denial or empty constraints**: System rejects the query immediately; no data is returned

#### Register UsageType

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-usecase-register-usage-type`

**Actor**: `cpt-cf-usage-collector-actor-platform-operator`

**Preconditions**:

- Actor is authenticated with a valid SecurityContext with operator-level permissions
- The UsageTypeGtsId is unique across the deployment

**Main Flow**:

1. Operator defines the registration payload: the UsageTypeGtsId (`gts_id`) and the closed `metadata_fields` list (array of strings naming every metadata key the UsageType will accept)
2. Operator submits the definition via the API
3. System authorizes the request via PDP and validates: (a) `gts_id` is well-formed; (b) `gts_id` begins with a reserved counter/gauge semantics prefix — otherwise the request is rejected with an actionable validation error; (c) `metadata_fields` is well-formed (an array of unique non-empty strings); (d) the `gts_id` is not already present in the catalog
4. System persists the UsageType in the catalog
5. Operator configures PDP authorization policies declaring which calling-gear identities are permitted to emit records referencing this UsageType, and for which tenants (the PDP reads the calling-gear identity from the platform-resolved `SecurityContext` at emit time)
6. System confirms successful registration

**Postconditions**:

- The new UsageType is immediately available for ingestion across all tenants; calling gears can emit records referencing it by `gts_id`
- PDP policies are in effect; unauthorized callers are rejected when attempting to emit records referencing this UsageType

**Alternative Flows**:

- **Duplicate UsageTypeGtsId**: System rejects registration with an actionable conflict error; no UsageType is created
- **Bad semantics prefix**: System rejects registration with an actionable validation error when `gts_id` does not begin with a reserved counter/gauge semantics prefix; no UsageType is created
- **Invalid `metadata_fields`**: System rejects registration with an actionable validation error when `metadata_fields` is malformed (non-array, contains duplicates, contains empty strings); no UsageType is created
- **PDP denial**: System rejects the registration before any change is made

#### Delete UsageType

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-usecase-delete-usage-type`

**Actor**: `cpt-cf-usage-collector-actor-platform-operator`

**Preconditions**:

- Actor is authenticated with a valid SecurityContext with operator-level permissions
- A UsageType with the specified `gts_id` exists in the catalog
- The deletion is referential — it cannot proceed while any usage row still references the target UsageType

**Main Flow**:

1. Operator submits a deletion request specifying the UsageType's `gts_id`
2. System authorizes the request via PDP
3. System removes the UsageType from the catalog; deletion is blocked while any usage row still references the target UsageType
4. System confirms successful deletion

**Postconditions**:

- The UsageType's `gts_id` is no longer registered; any subsequent ingestion attempt referencing it is rejected by `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`
- The UsageType's `gts_id` becomes available for re-registration

**Alternative Flows**:

- **UsageType not found**: System returns an actionable not-found error
- **UsageType still referenced**: System returns an actionable conflict error (usage_type still referenced by usage records); the UsageType remains in the catalog
- **PDP denial**: System rejects the deletion before any change is made

#### Query Raw Usage Records

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-usecase-query-raw`

**Actor**: `cpt-cf-usage-collector-actor-usage-consumer`, `cpt-cf-usage-collector-actor-tenant-admin`

**Preconditions**:

- Actor is authenticated with a valid SecurityContext

**Main Flow**:

1. Consumer sends a raw-record query specifying a mandatory time range and optional product-level narrowing criteria
2. System authorizes the query via PDP; PDP-returned constraints define the authorization boundary and user-supplied filters are applied in addition, only further narrowing the result set
3. System returns a page of raw records when authorized records exist

**Postconditions**:

- Consumer receives raw records within the intersection of PDP-authorized scope and user-supplied filters
- Additional pages are available through the paging behavior defined by the public contract

**Alternative Flows**:

- **No data in range or scope**: System returns an empty page (not an error)
- **PDP denial or empty constraints**: System rejects the query immediately; no data is returned
- **Invalid paging request**: System returns an actionable error

#### Deactivate a Usage Event

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-usecase-deactivate-event`

**Actor**: `cpt-cf-usage-collector-actor-platform-operator`

**Preconditions**:

- Actor is authenticated with a valid SecurityContext with operator-level permissions
- The target usage event exists and is active

**Main Flow**:

1. Operator submits a deactivation request identifying the target event
2. System authorizes the request via PDP
3. System transitions the event's `status` to `inactive`; no other property is modified

**Postconditions**:

- The event carries `status = inactive`; all other properties (including `tenant`, `created_at`, `idempotency_key`, `value`, referenced UsageType, resource, subject, and metadata) are unchanged
- Inactive events remain queryable and are distinguishable from active records by downstream consumers

**Alternative Flows**:

- **Event not found**: System returns a not-found error
- **Target event already inactive**: System rejects the request with an actionable error; deactivation is one-way and not applicable to an already-inactive record
- **PDP denial**: System rejects the request before any change is made

#### Compensate Previously Reported Usage

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-usecase-compensate-previously-reported-usage`

**Actor**: `cpt-cf-usage-collector-actor-usage-source`

**Preconditions**:

- The calling gear is an authenticated usage source with PDP authorization to emit records for the target `(tenant_id, gts_id)`
- A prior original usage row `R` exists for the target `(tenant_id, gts_id)` on a `counter` UsageType, and `R.status = active`

**Trigger**: The calling gear observes a real give-back of measured consumption (e.g., capacity refund, partial revocation, corrective downward adjustment) that partially reverses the value of `R` but does not justify a whole-row retraction of `R`.

**Main Flow**:

1. Calling gear constructs a new compensation record pointing at `R` with a strictly-negative `value`, the same `(tenant_id, gts_id)` as `R`, an idempotency key (mandatory), and the platform-resolved `SecurityContext`.
2. Calling gear submits the record via the **same ingestion path** used for `usage` rows — there is no separate compensate endpoint (cross-reference `cpt-cf-usage-collector-fr-usage-compensation`).
3. System authorizes the emission via PDP and validates the record against the UsageType-semantics × row-classification matrix and the compensation pointer contract (referenced row exists, is an original usage row, shares the full identity tuple `(tenant_id, gts_id, resource_ref, subject_ref)` — `subject_ref` presence is part of the identity — and is `active`).
4. System accepts the compensation record and appends it to the store.
5. The record becomes part of aggregation results: `SUM(value)` for `(tenant_id, gts_id)` over `active` rows is reduced by `|value|`; `COUNT` / `MIN` / `MAX` / `AVG` continue to operate over original usage rows only and are unaffected by the compensation.

**Postconditions**:

- A new active compensation record is persisted; `R` remains active and unchanged.
- The net `SUM(value)` for `(tenant_id, gts_id)` is reduced by `|value|`.
- The compensation record is queryable through the raw and aggregated query surfaces and is part of the audit history.

**Alternative Flows**:

- **Gauge-compensation rejected**: the referenced UsageType is `gauge` semantics; ingestion rejects the record with an actionable error (compensation is counter-only).
- **Invalid compensation pointer rejected**: the referenced row is missing, is itself a compensation row, has a mismatching `(tenant_id, gts_id, resource_ref, subject_ref)` identity tuple (including a `subject_ref` presence mismatch), or is `inactive`; ingestion rejects the record with an actionable error.
- **Deactivating-row rejected (concurrency)**: the referenced original usage row is being deactivated concurrently; the "must be active" check rejects the compensation without distributed coordination.
- **Non-negative compensation value rejected**: the supplied `value` is zero or positive on a `counter` UsageType; ingestion rejects the record with an actionable error.
- **Authorization denied**: PDP denies the emission; the record is rejected immediately and never persisted.
- **Idempotency conflict / retry**: an exact-equality re-submission under the same idempotency key is silently deduplicated; a same-key submission whose content differs is rejected with an actionable conflict error (cross-reference `cpt-cf-usage-collector-fr-idempotency`).
- **Cascade on later deactivation of `R`**: if `R` is subsequently deactivated by an operator, the depth-1 cascade defined by `cpt-cf-usage-collector-fr-event-deactivation` flips this compensation row to `inactive` in the same one-way step.

## 9. Acceptance Criteria

The following definitions apply to every numeric acceptance criterion in this section that references a load condition or a latency tolerance. They replace the prior informal terms "normal load", "normal operation", and "linear throughput scaling" across the PRD and anchor every test condition on a single, deterministic envelope.

- **Load envelope ("normal load" / "normal operation")** — the steady-state operating envelope defined by `cpt-cf-usage-collector-nfr-throughput-profile`: sustained ingestion ≥ 10,000 records/sec, ≥ 100 concurrent aggregation queries, ≥ 700,000,000 accepted ingestion calls per 24-hour day, with no active burst in progress unless a criterion explicitly references the burst case. The burst case is ≤ 30,000 records/sec for ≤ 5 minutes per 60-minute window.
- **Steady-state measurement window** — a contiguous window of ≥ 30 minutes during which the load envelope above is sustained; p95 figures are computed over this window and the trailing 30-minute window is reported alongside any single-sample p95.
- **Latency tolerance** — every p95 latency criterion in [§9](#9-acceptance-criteria) carries a measurement tolerance of ±10% on the stated p95 value, applied per steady-state measurement window; the trailing 30-minute trend **MUST** remain at or below the stated p95 value.
- **Burst tolerance** — for the burst case of `cpt-cf-usage-collector-nfr-throughput-profile`, the p95 ingestion-latency bound (200ms with ±10% tolerance) applies for the duration of the burst (≤ 5 minutes) and the trailing 60-minute window MUST contain at most one burst event.

The functional and non-functional acceptance bullets below evaluate the requirements defined in [§5](#5-functional-requirements) and [§6](#6-non-functional-requirements) against the load envelope and measurement rules established above.

- [ ] Authenticated usage sources can submit usage records attributed to a tenant, resource, optional subject, and a registered UsageType; an accepted record becomes durably retained and queryable through the raw and aggregated query surfaces (cross-reference `cpt-cf-usage-collector-fr-ingestion`)
- [ ] Gauge UsageType records are stored as-is without monotonicity enforcement and without delta accumulation; consecutive gauge values for the same `(tenant, usage_type)` may rise or fall arbitrarily; idempotent dedup by idempotency key still applies; querying a gauge UsageType returns the persisted point-in-time values rather than an accumulated total (cross-reference `cpt-cf-usage-collector-fr-gauge-semantics`)
- [ ] An exact-equality re-submission under the same idempotency key results in a single stored record (silent dedup), while a same-key submission whose content differs is rejected with a duplicate-submission conflict signal rather than silently dropped; the dedup key tuple is preserved across retention so the window stays unbounded (cross-reference `cpt-cf-usage-collector-fr-idempotency`)
- [ ] Records submitted without an idempotency key are rejected with an actionable error
- [ ] Counter records with negative values are rejected at ingestion
- [ ] Incoming usage records include an explicit tenant attribute; the platform PDP validates that the authenticated caller is authorized to emit records for the specified tenant before the record is accepted, and the gateway independently validates tenant attribution on ingest as a defense-in-depth check
- [ ] Every usage record includes resource attribution (resource ID and type); records without either field are rejected
- [ ] Usage records can optionally include an explicit subject attribute (subject ID and type); when present, the platform PDP validates that the authenticated caller is authorized to emit records attributed to the specified subject before the record is accepted; when absent, PDP subject validation is skipped
- [ ] Authorization failures are surfaced immediately to the caller; no record is persisted on denial
- [ ] Tenant isolation is enforced via PDP: a caller never receives a tenant's usage data — for reads or writes — without an explicit PDP authorization for that tenant; same-tenant, parent→subtenant, and platform-administrative scopes are each authorized independently
- [ ] Aggregation queries require exactly one UsageType and a time range; requests omitting the UsageType or supplying more than one UsageType are rejected with an actionable error
- [ ] Aggregation queries return correct results for the specified UsageType and time range, with correct additional filtering by tenant (optional), subject, and resource when specified
- [ ] Aggregation results can be grouped by any combination of time bucket, tenant, subject, and resource
- [ ] Raw usage queries support filtering by time range (mandatory) and optionally by tenant, usage_type, subject, and resource
- [ ] Query authorization is enforced via PDP decision and constraint enforcement; unauthorized queries are rejected and PDP-returned constraints narrow the result scope
- [ ] The gear works with any registered plugin (e.g., ClickHouse, TimescaleDB) without code changes to the core gear
- [ ] Metadata attached to a usage record is persisted as-is and returned in query results without modification
- [ ] Usage records with metadata exceeding the configured size limit are rejected with an actionable error
- [ ] Individual usage events can be deactivated: `status` transitions from active to `inactive` with no other property changes; inactive events remain queryable and are distinguishable from active records by downstream consumers; deactivation is one-way (no reactivation operation is exposed) and rejects already-inactive targets
- [ ] Compensation entries are accepted only on `counter` UsageTypes; a compensation entry referencing a `gauge` UsageType is rejected at ingestion with an actionable error (cross-reference `cpt-cf-usage-collector-fr-usage-compensation`)
- [ ] Compensation entries on a `counter` UsageType require `value < 0`; non-negative `value` (zero or positive) is rejected at ingestion with an actionable error (cross-reference `cpt-cf-usage-collector-fr-usage-compensation`)
- [ ] The compensation pointer to the corrected usage row is validated at ingestion: the referenced record MUST exist, MUST be an original (non-compensation) row classification, MUST share the full identity tuple `(tenant_id, gts_id, resource_ref, subject_ref)` with the incoming compensation (`subject_ref` presence is part of the identity — `None` vs `Some(_)` is a scope mismatch), and MUST be `active`; any failure rejects the compensation with an actionable error (cross-reference `cpt-cf-usage-collector-fr-usage-compensation`)
- [ ] Deactivating a `usage` row cascades depth-1 to its active referencing `compensation` rows: those compensations are flipped to `inactive` in the same one-way step so the post-cascade `SUM` returns to the state held before either the usage record or its compensations were accepted (cross-reference `cpt-cf-usage-collector-fr-event-deactivation`, `cpt-cf-usage-collector-fr-usage-compensation`)
- [ ] Concurrency safety: a compensation referencing a `usage` row that is concurrently being deactivated is rejected by the L1 "referenced record must be active" check; no distributed coordination is required (cross-reference `cpt-cf-usage-collector-fr-usage-compensation`)
- [ ] Every compensation ingestion call carries a mandatory idempotency key: an exact-equality re-submission is silently deduplicated and a same-key content mismatch is rejected with a duplicate-submission conflict signal, with the dedup key tuple preserved across retention (cross-reference `cpt-cf-usage-collector-fr-usage-compensation`)
- [ ] Usage records whose `usage_type` field does not match a registered UsageType are rejected immediately with an actionable error before any record is accepted for delivery
- [ ] UsageTypes can be registered via API without code changes or service redeployment; the UsageTypeGtsId uniquely identifies a UsageType and duplicate identifiers are rejected; registration is PDP-authorized
- [ ] UsageType registration validates the supplied closed `metadata_fields` list (array of strings; ingest rejects records carrying any metadata key not in the list with an unknown metadata key signal) and validates the `gts_id` prefix against the reserved counter/gauge semantics prefixes — any other prefix is rejected at registration with an invalid usage-type-semantics signal; counter/gauge semantics is derived from the `gts_id` prefix and is not a separate registration field, trait, or catalog column
- [ ] UsageTypes can be deleted via API; deletion is blocked while referenced by any usage record (active or inactive, in any tenant); deletion is PDP-authorized
- [ ] The system maintains 99.95% monthly availability for ingestion endpoints
- [ ] The system sustains ingestion of at least 10,000 records/sec sample-mean under the `cpt-cf-usage-collector-nfr-throughput-profile` load envelope, with every 1-minute sample-mean ≥ 9,500 records/sec, measured over a ≥ 30-minute steady-state window
- [ ] Usage record ingestion completes within 200ms at p95 under the `cpt-cf-usage-collector-nfr-throughput-profile` load envelope, with the ±10% tolerance defined in §9.0 (single-window p95 ≤ 220ms accepted only when the trailing 30-minute p95 remains ≤ 200ms)
- [ ] Aggregation queries over a 30-day range for a single tenant complete within 500ms at p95 under the `cpt-cf-usage-collector-nfr-throughput-profile` load envelope, with the ±10% tolerance defined in §9.0 (single-window p95 ≤ 550ms accepted only when the trailing 30-minute p95 remains ≤ 500ms)
- [ ] Ingestion p95 latency remains within the bound from `cpt-cf-usage-collector-nfr-ingestion-latency` (p95 ≤ 200ms with the §9.0 ±10% tolerance) while ≥ 100 concurrent aggregation queries are executing inside the `cpt-cf-usage-collector-nfr-throughput-profile` envelope
- [ ] Usage records submitted by a `cpt-cf-usage-collector-actor-usage-source` are accepted only after PDP authorizes the authenticated caller (calling-gear identity from `SecurityContext`) for the supplied tenant, resource, subject (if any), and referenced UsageType; unauthenticated or unauthorized submissions are rejected immediately with no partial persistence
- [ ] Plugin SPI, SDK trait, and REST API public surfaces remain stable within a major version: a consumer compiled or wired against major version N **MUST** continue to function unchanged against every minor and patch release of major version N; at most one prior major version is supported concurrently per surface; within a major version only additive changes (new endpoints, new optional fields, new methods with defaults) are accepted (cross-reference `cpt-cf-usage-collector-nfr-plugin-contract-stability`)
- [ ] All authentication is performed by the ToolKit gateway upstream of the collector; the gear does not implement local credential validation, MFA, SSO/federation, session management, or credential issuance, does not consume any credential-resolution contract, and rejects every REST or SDK call that arrives without a platform-resolved `SecurityContext`
- [ ] Persisted gear data is limited to opaque platform identifiers, operational telemetry, and opaque caller-supplied metadata; the gear performs no decoding of identifiers to natural persons, and integrator-facing documentation states the prohibition on placing PII, payment, health, or credential data in metadata
- [ ] Every API operation contributes a correlation identifier that reconciles gear activity with platform gateway access logs and platform audit infrastructure; no gear-local audit log is maintained in v1
- [ ] Every accepted ingestion, query, deactivation, and UsageType lifecycle operation is attributable to an authenticated caller identity recorded in the platform audit trail; anonymous and synthesized identities are rejected
- [ ] Privacy by Design principles are applied at PRD level (data minimization, purpose limitation, storage limitation delegated to plugin, privacy by default through PDP, pseudonymization via opaque identifiers) and documented for downstream review
- [ ] Data-ownership model is recorded: tenant administrator owns tenant usage data, platform operator stewards the UsageType catalog and storage-plugin selection, and the Usage Collector gear acts as custodian; third-party access flows exclusively through PDP-authorized public read surfaces
- [ ] Data-quality guarantees are verifiable: semantics-invariant enforcement, mandatory attribution, ingestion-ack latency bounded by `cpt-cf-usage-collector-nfr-ingestion-latency`, queryability governed separately by `cpt-cf-usage-collector-nfr-query-freshness` (plugin-bound; no read-your-writes assumption against the query surfaces; ack is the surface for same-request outcome), gateway-level validation, and absence of in-gear amendment (corrections expressed as deactivation plus re-emission)
- [ ] The query-freshness consistency contract is verifiable: the gear floor publishes ingestion ack durability and dedup-tuple visibility on the ingestion path, declares the Query SPI (raw, aggregated, catalog) eventually consistent with no upper bound at the gear floor, and obliges every active plugin's deployment guide to publish its actual consistency profile (`cpt-cf-usage-collector-nfr-query-freshness`); plugin-specific ceilings are verified against each plugin's published profile separately
- [ ] Data-lifecycle delegation is documented: retention, archival, purging, migration, and historical access are governed by the active storage plugin's deployment profile and the platform governance layer; the gear's surface preserves historical query access within the plugin-provided retention window
- [ ] Standards, legal, and compliance applicability is declared at PRD level: alignment with the platform security baseline and OpenAPI 3 interoperability; PCI DSS, HIPAA, and SOX explicitly not applicable; consent management, data-subject-rights, terms-of-service, and privacy-policy duties delegated to the platform identity, legal, and governance layers; data residency delegated to platform topology and operator-selected plugin deployment profile
- [ ] Sustained ingestion of ≥ 10,000 records/sec and burst ingestion of ≥ 30,000 records/sec for ≤ 5 minutes per 60-minute window are sustainable without breaching ingestion p95 latency; ≥ 100 concurrent aggregation queries are sustainable without breaching query p95 latency or degrading ingestion p95; ≥ 700,000,000 accepted ingestion calls per 24-hour day are sustainable at the sustained rate
- [ ] Usage Collector domain metrics are integrated into shared platform dashboards and alert routing, with operator treatment for ingestion latency, ingestion error rate, query latency, PDP error rate, and storage-plugin readiness; every accepted and rejected API operation emits a structured log record carrying the inbound `correlation_id` unchanged

## 10. Dependencies

| Dependency     | Description                                                                            | Criticality |
| -------------- | -------------------------------------------------------------------------------------- | ----------- |
| authz-resolver | Platform PDP; authorizes every ingestion, query, and operator-write operation          | p1          |
| gts-registry   | Platform registry/orchestration dependency used for active storage extension selection | p1          |

## 11. Assumptions

| Assumption                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | Owner                                                                      | Validation                                                                                                              |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| At least one plugin (e.g., a ClickHouse or TimescaleDB storage backend) is deployed alongside the gear                                                                                                                                                                                                                                                                                                                                                                                                                       | Platform Infrastructure / Operator                                         | Verified at gear startup via platform storage-extension resolution; readiness fails if no active plugin resolves      |
| Platform documentation and operations channels are available for publishing Usage Collector quickstarts, API references, and support runbooks before release candidate                                                                                                                                                                                                                                                                                                                                                         | Usage Collector Maintainers / Platform Documentation / Platform Operations | Verified during release-readiness review                                                                                |
| The gateway delivers an authenticated security context to the usage-collector gear on every call; the gear rejects any request that arrives without a platform-resolved security context                                                                                                                                                                                                                                                                                                                                   | Platform Identity / Platform Security                                      | Verified by gateway integration tests against the usage-collector gear                                                |
| Platform gateway access logs and platform audit infrastructure are available to record authentication, authorization, ingestion, query, and operator-write outcomes and accept correlation identifiers emitted by the Usage Collector                                                                                                                                                                                                                                                                                          | Platform Operations / Platform Audit Owner                                 | Verified by end-to-end correlation between gear logs and platform audit records before release candidate              |
| Operator-selected storage plugin deployment topology meets the deployment's data residency, sovereignty, retention, and disaster-recovery obligations for tenant usage data                                                                                                                                                                                                                                                                                                                                                    | Platform Operator / Plugin Authors                                         | Verified during operator onboarding and at storage-plugin readiness review                                              |
| Initial release establishes the launch capacity baseline (10,000 records/sec sustained, 30,000 records/sec burst, 100 concurrent aggregation queries, 10,000 tenants, 10,000 registered UsageTypes); no prior historical workload data exists at launch                                                                                                                                                                                                                                                                       | Usage Collector Maintainers / Platform Operations                          | Validated by launch load tests against representative plugin backends                                                   |
| Platform monitoring and log infrastructure are available to host the observable signals expected by the operational visibility NFR                                                                                                                                                                                                                                                                                                                                                                                              | Platform Operations                                                        | Verified during operations readiness review before production release candidate                                         |
| The §9.0 load and measurement definitions (load envelope anchored on `cpt-cf-usage-collector-nfr-throughput-profile`, ≥ 30-minute steady-state measurement window, ±10% latency tolerance) are the single source of truth for every numeric acceptance criterion in [§9](#9-acceptance-criteria) and supersede the prior informal terms "normal load" and "normal operation" wherever they appeared in earlier PRD revisions                                                                                                    | Usage Collector Maintainers / Platform Operations                          | Verified during load-test plan review and release-readiness review                                                      |

## 12. Risks

| Risk                                                                                                                                                                                                                                                                                                                           | Impact                                                                                                                                 | Mitigation                                                                                                                                                                                                                                                                                                                                                  |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| High-cardinality aggregation exceeds 500ms p95 query latency                                                                                                                                                                                                                                                                   | Slow dashboard/billing queries                                                                                                         | See DESIGN.md for storage-extension acceleration and workload-isolation strategy                                                                                                                                                                                                                                                                            |
| v1 lacks gear-emitted audit events for operator-write paths (UsageType registration, UsageType deletion, individual event deactivation); reliance is on platform gateway access logs and platform audit infrastructure with gear-emitted correlation identifiers until the deferred audit-emission capability is delivered | Reduced gear-local forensic detail for operator writes; downstream compliance reporting depends on platform-level audit completeness | Document the deferral, surface correlation identifiers, and track the deferred audit-emission capability against the [§4.2](#42-out-of-scope) Audit Events item for a future phase                                                                                                                                                                          |
| Data residency or sovereignty obligations could be violated if the operator-selected storage plugin is deployed outside the permitted region or topology                                                                                                                                                                       | Compliance and contractual breach for tenants subject to residency commitments                                                         | Operator onboarding documents the residency expectations; plugin deployment profile reviewed at readiness; cross-reference [§4.2](#42-out-of-scope) deferred Multi-Region Replication                                                                                                                                                                       |

## 13. Open Questions

No open questions.

## 14. Traceability

**Design**: [DESIGN.md](./DESIGN.md)

**ADRs**: see DESIGN §5 ADR Inventory
