---
cpt:
  version: "3.5"
  changelog:
    - version: "3.5"
      date: "2026-05-10"
      changes:
        - "Document the gateway-side authz module location (RESEARCH-diverge issue H): the §3 `cpt-cf-usage-collector-algo-query-api-authz-delegate` algorithm is implemented in `usage-collector/src/domain/authz.rs`; `Service` calls it before each query and embeds the resulting `AccessScope` into `AggregationQuery.scope` / `RawQuery.scope`. Document the REST API license-feature gate and OpenAPI 503 declaration drift (RESEARCH-diverge issue I): every `/usage-collector/v1/*` endpoint registered by `routes.rs` is gated behind `LicenseFeature` `gts.cf.core.lic.feat.v1~cf.core.global.base.v1` via `OperationBuilder::require_license_features::<License>([])` — added to §5 Gateway Aggregated/Raw DoD bodies and §6 AC. The gateway's runtime 503 emission for `service_unavailable` (per `inst-agg-8c` / `inst-raw-8b`) is produced by the canonical Problem mapper at the handler boundary; the OpenAPI registry currently declares only the explicitly-registered `error_400` / `error_403` / `error_500` responses for the query routes, so the published OpenAPI document does not advertise the runtime 503 — captured as MAINT-FDESIGN-002-02 in §5 Known Limitations."
    - version: "3.4"
      date: "2026-05-10"
      changes:
        - "Drop `ctx: &SecurityContext` from the storage-plugin query trait operations (RESEARCH-diverge issue E). The gateway now compiles the PDP-derived `AccessScope` and embeds it in `AggregationQuery.scope` / `RawQuery.scope` (per `inst-agg-7` / `inst-raw-7`), so `UsageCollectorPluginClientV1::query_aggregated` and `::query_raw` take no separate `SecurityContext` parameter. Updates: `inst-sdk-8` and `inst-sdk-9` (trait signatures); `inst-agg-8` and `inst-raw-8` (gateway → plugin call labels); `inst-noop-1` and `inst-noop-2` (noop plugin steps no longer mention an ignored `ctx`)."
    - version: "3.3"
      date: "2026-05-10"
      changes:
        - "Complete v3.2 canonical-taxonomy alignment in the §2 gateway flows that consume `inst-plugin-contract-3` — v3.2 scoped its rewrite to the plugin contract / DoD / AC / OPS sections but missed the gateway flow steps that still referenced the nonexistent `QueryResultTooLarge` variant. Drop the gateway-side 400 'query too broad' shortcut (the actual handler at `usage-collector/src/api/rest/handlers.rs:287-301` forwards canonical status codes via `domain_error_to_problem` → `canonical_error_to_problem` → `e.status_code()` and never special-cases the variant). Updates: `inst-agg-8b` rewritten as a no-op fall-through note that preserves the ID for traceability and points at `inst-agg-8c`'s canonical Problem mapping; `inst-agg-8c` qualifier `[that is not QueryResultTooLarge]` removed; `inst-raw-8b` qualifier `[that is not QueryResultTooLarge]` removed (raw queries do not enforce `MAX_AGG_ROWS`, so the qualifier was doubly stale); §6 AC line for `Err(ResourceExhausted)` updated from 'returns 400 query too broad' to 'mapped via the canonical Problem flow per inst-agg-8c — there is no gateway-side 400 shortcut'."
    - version: "3.2"
      date: "2026-05-10"
      changes:
        - "Replace nonexistent `UsageCollectorError::QueryResultTooLarge` variant with the canonical `ResourceExhausted` variant (built via `UsageRecordError::resource_exhausted(\"query result too large\")`). `UsageCollectorError = CanonicalError` exposes no `QueryResultTooLarge` builder; `ResourceExhausted` is the canonical carrier for quota / size-limit violations in the platform error taxonomy. Updates: `inst-plugin-contract-3` (§3 plugin contract), `cpt-cf-usage-collector-dod-query-api-plugin-contract` (§5 DoD body), §6 AC line 'plugin returns Err(QueryResultTooLarge)', and OPS-FDESIGN-002 `MAX_AGG_ROWS` constant description in §7."
    - version: "3.1"
      date: "2026-05-03"
      changes:
        - "Drop unimplementable cursor TTL spec: removed inst-sdk-6a (410 on expired cursor), removed CURSOR_TTL from OPS-FDESIGN-002, removed TTL references from inst-sdk-6 and REL-FDESIGN-003, added Known Limitation MAINT-FDESIGN-003-3; CursorV1 carries no issued_at timestamp so TTL enforcement is impossible"
        - "Fix raw query wire format example (MAINT-FDESIGN-002): corrected UsageRecord field names (id→module+tenant_id, usage_type→metric, quantity→value, dropped non-existent source field)"
    - version: "3.0"
      date: "2026-05-03"
      changes:
        - "Type alignment with modkit-odata: replaced bespoke Cursor type with CursorV1 (modkit-odata) and PagedResult<T> with Page<T>/PageInfo (modkit-odata) in inst-sdk-6, inst-sdk-7, inst-sdk-9, inst-noop-2, inst-raw-8, inst-raw-9, DoD SDK types, DoD gateway raw, acceptance criteria, REL-FDESIGN-003, and TEST-FDESIGN-001; updated MAINT-FDESIGN-002 raw query example to reflect Page<T> wire format"
    - version: "2.9"
      date: "2026-04-30"
      changes:
        - "ACs & §7 NFR checklist: appended PERF-FDESIGN-004 deferral annotation to performance AC items 1-2; added multi-page cursor traversal and AccessScope scope-filtering AC items; added §7 entries for UX-FDESIGN-002, MAINT-FDESIGN-001, MAINT-FDESIGN-002 (with examples), MAINT-FDESIGN-003, TEST-FDESIGN-001; expanded INT-FDESIGN-003 with PDP client auth and version compatibility sub-items; expanded Known Limitations (SEM-FDESIGN-005, TEST-FDESIGN-003-01, DOC-FDESIGN-001-01, INT-FDESIGN-003-01, MAINT-FDESIGN-002-01, MAINT-FDESIGN-003-01)"
    - version: "2.8"
      date: "2026-04-30"
      changes:
        - "CDSL format: converted plugin-contract items 1–5 from plain prose to CDSL instructions with checkboxes, p2 markers, and inst-plugin-contract-N slugs; added Input/Output/Steps sub-structure (CDSL-FORMAT-001)"
    - version: "2.7"
      date: "2026-04-30"
      changes:
        - "Cursor reliability: added CURSOR_TTL constant to OPS-FDESIGN-002; added TTL-expiry 410 step (inst-sdk-6a); replaced delete-consistency caveat with positive specification; added cursor encoding design note (REL-FDESIGN-003-01, REL-FDESIGN-003-02, DATA-FDESIGN-003-01)"
    - version: "2.6"
      date: "2026-04-30"
      changes:
        - "Renamed metric → usage_type in AggregationQuery and RawQuery (SEM-FDESIGN-004)"
    - version: "2.5"
      date: "2026-04-30"
      changes:
        - "Remediation (16 issues): add subject_type to RawQuery (inst-sdk-7); resolve plugin contract contradiction via QueryResultTooLarge variant (Option B); add cursor stability semantics to inst-sdk-6 and fix REL-FDESIGN-003; explicit AND-within-group in inst-authz-4 and plugin-contract item 2; add 503 body spec to inst-agg-8c/inst-raw-8b; add LOG to inst-authz-3b; fix inst-authz-3b step numbering note and CDSL bold formatting; add §7 PERF-FDESIGN-004 entry and §6 deferral bullet; expand §7 COMPL to per-sub-item N/A (GDPR, SOC 2); add §7 XSS/cmd-injection/path-traversal N/A; add §7 PERF-FDESIGN-002 N/A; add §7 INT-FDESIGN-002 N/A; add §7 DATA-FDESIGN-001 deferral; DECOMPOSITION F3 backfill: constraint or-of-ands-preservation, 5 domain types, principle pluggable-storage"
    - version: "2.4"
      date: "2026-04-30"
      changes:
        - "Phase 4 remediation: PERF-FDESIGN-004 entry added to §7 with 4 sub-items; §6 deferral bullet added; COMPL §7 entry replaced with per-sub-item GDPR/SOC2 N/A justification"
    - version: "2.3"
      date: "2026-04-30"
      changes:
        - "v2.3: Phase 3 — applied Issue 6 (authz AND-within-group), Issue 7 (503 body), Issue 8 (PDP LOG), Issue 12 (ID-encoding note)."
    - version: "2.2"
      date: "2026-04-30"
      changes:
        - "v2.2: added subject_type to RawQuery (inst-sdk-7, inst-raw-7); strengthened plugin contract item 3 with QueryResultTooLarge error variant; added cursor stability semantics to inst-sdk-6 and REL-FDESIGN-003"
    - version: "2.1"
      date: "2026-04-30"
      changes:
        - "Post-analysis remediation (6 issues): constraint cpt-cf-usage-collector-constraint-or-of-ands-preservation backfilled to DESIGN; AggregationFn/BucketSize/GroupByDimension/Cursor added to DESIGN domain model; performance acceptance criteria tied to nfr-query-latency; dod-query-api-plugin-contract added; CDSL step 3b renumbered; SEC-FDESIGN-001 N/A noted"
    - version: "2.0"
      date: "2026-04-29"
      changes:
        - "Remediation: 40 analysis issues fixed (SEC, REL, ARCH, SEM, DATA, PERF, INT, OPS, TEST)"
    - version: "1.0"
      date: "2026-04-29"
      changes:
        - "Initial feature specification"
---

# Feature: Usage Query API

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Aggregated Usage Query](#aggregated-usage-query)
  - [Raw Usage Query](#raw-usage-query)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Authorize and Compile Scope](#authorize-and-compile-scope)
  - [SDK Type Additions](#sdk-type-additions)
  - [Noop Plugin Query Stubs](#noop-plugin-query-stubs)
  - [Plugin Trait Contract Requirements](#plugin-trait-contract-requirements)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [SDK Types and Trait Operations](#sdk-types-and-trait-operations)
  - [Noop Plugin Query Stubs](#noop-plugin-query-stubs-1)
  - [Gateway Aggregated Query Handler](#gateway-aggregated-query-handler)
  - [Gateway Raw Query Handler](#gateway-raw-query-handler)
  - [Plugin Trait Contract](#plugin-trait-contract)
- [6. Acceptance Criteria](#6-acceptance-criteria)
  - [Performance Acceptance Criteria](#performance-acceptance-criteria)
- [7. Non-Applicability Notes](#7-non-applicability-notes)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-featstatus-query-api`

<!-- reference to DECOMPOSITION entry -->
- [x] `p1` - `cpt-cf-usage-collector-feature-query-api`

## 1. Feature Context

### 1.1 Overview

Adds aggregated and raw usage query capability to the gateway: new SDK types and plugin trait operations, a trivial noop plugin implementation, and two new gateway endpoints that authorize queries via the platform PDP and delegate to the active storage plugin.

### 1.2 Purpose

Enables authorized usage consumers and tenant administrators to retrieve aggregated statistics and paginated raw records from the storage backend. Covers the SDK trait boundary and gateway authorization layer. Ingest pipeline (Feature 1), production storage backend (Feature 4), and watermark metadata query (Feature 8) are out of scope.

**Requirements**: `cpt-cf-usage-collector-fr-query-aggregation`, `cpt-cf-usage-collector-fr-query-raw`, `cpt-cf-usage-collector-nfr-workload-isolation`

**Principles**: `cpt-cf-usage-collector-principle-fail-closed`, `cpt-cf-usage-collector-principle-tenant-from-ctx`, `cpt-cf-usage-collector-principle-pluggable-storage`

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-usage-collector-actor-usage-consumer` | Calls aggregated and raw query endpoints to retrieve usage data |
| `cpt-cf-usage-collector-actor-tenant-admin` | Calls aggregated and raw query endpoints to audit tenant usage |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Dependencies**: `cpt-cf-usage-collector-feature-sdk-and-ingest-core`

## 2. Actor Flows (CDSL)

### Aggregated Usage Query

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-query-api-aggregated`

**Actor**: `cpt-cf-usage-collector-actor-usage-consumer`, `cpt-cf-usage-collector-actor-tenant-admin`

**Sequences**: `cpt-cf-usage-collector-seq-query-aggregated`

**Success Scenarios**:
- Consumer receives 200 with an array of `AggregationResult` rows matching the requested function and GROUP BY dimensions (empty array is a valid success when no data falls in the range)

**Error Scenarios**:
- PDP denies or returns empty constraints → 403 Forbidden (fail closed; never fall through to allow-all)
- PDP non-Denied error (network, timeout, internal) → 403 Forbidden (fail-closed)
- 403 response: RFC 9457 `application/problem+json` with `context = {"error": "forbidden"}` — generic message only; MUST NOT include PDP error details, constraint names, or policy names
- Mandatory `from` or `to` parameter missing or malformed → 400 Bad Request
- `group_by` includes `time_bucket` but `bucket_size` is absent → 400 Bad Request

**Steps**:
1. [x] - `p1` - Consumer: API: GET /usage-collector/v1/aggregated (?fn=, &from=, &to=, &group_by[]=, &bucket_size=, &usage_type=, &subject_id=, &subject_type=, &resource_id=, &resource_type=, &source=) - `inst-agg-1`
2. [x] - `p1` - Gateway: derive tenant_id from SecurityContext; never accept tenant_id as a query parameter - `inst-agg-2`
3. [x] - `p1` - Gateway: validate mandatory parameters: fn and time range (from, to) must be present; bucket_size must be present when group_by includes time_bucket; validate max-length of string filter fields (usage_type, resource_type, subject_type, source); reject with 400 if any exceeds MAX_FILTER_STRING_LEN - `inst-agg-3`
   1. [x] - `p1` - **IF** `from >= to` → **RETURN** 400 validation error (time range must be strictly ascending) - `inst-agg-3a`
   2. [x] - `p1` - **IF** `(to - from) > MAX_QUERY_TIME_RANGE` → **RETURN** 400 validation error (range exceeds configured limit; `MAX_QUERY_TIME_RANGE` defined in §7 OPS-FDESIGN-002) - `inst-agg-3b`
4. [x] - `p1` - **IF** validation fails - `inst-agg-4`
   1. [x] - `p1` - **RETURN** 400 Bad Request as RFC 9457 `application/problem+json`; the `context` extension member is `{"error": "<message>", "code": "VALIDATION_ERROR", "details": ["<field>: <reason>", ...]}` - `inst-agg-4a`
5. [x] - `p1` - Gateway: invoke authorize-and-compile-scope (`cpt-cf-usage-collector-algo-query-api-authz-delegate`) for USAGE_RECORD_READ / LIST - `inst-agg-5`
6. [x] - `p1` - **IF** authorization failed - `inst-agg-6`
   1. [x] - `p1` - **RETURN** 403 Forbidden - `inst-agg-6a`
7. [x] - `p1` - Gateway: build AggregationQuery (scope from compiled AccessScope, time_range, function, group_by, bucket_size, user-supplied optional filters) - `inst-agg-7`
   > The HTTP query parameter `usage_type=` maps directly to the `usage_type` field of `AggregationQuery`. No translation is required.
8. [x] - `p1` - Gateway: Plugin: query_aggregated(AggregationQuery) — the gateway has already compiled the PDP-derived `AccessScope` into `AggregationQuery.scope` (see `inst-agg-7`); the plugin contract takes no separate `SecurityContext` - `inst-agg-8`
   1. [x] - `p1` - The gateway does NOT special-case `Err(ResourceExhausted)` (e.g., `MAX_AGG_ROWS` exceeded per `inst-plugin-contract-3`); the canonical Problem mapping in `inst-agg-8c` applies — `inst-agg-8b`
   2. [x] - `p1` - **IF** plugin returns `Err(e)` → **LOG** at `ERROR` level with correlation ID (no PII) → **RETURN** `503 Service Unavailable` as RFC 9457 `application/problem+json` with `context = {"error": "service_unavailable", "correlation_id": "<id>"}` ; the `correlation_id` field MUST match the value logged at `ERROR` level. - `inst-agg-8c`
9. [x] - `p1` - **RETURN** 200 OK with Vec<AggregationResult> - `inst-agg-9`

### Raw Usage Query

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-flow-query-api-raw`

**Actor**: `cpt-cf-usage-collector-actor-usage-consumer`, `cpt-cf-usage-collector-actor-tenant-admin`

**Sequences**: `cpt-cf-usage-collector-seq-query-raw`

**Success Scenarios**:
- Consumer receives 200 with a page of raw `UsageRecord` items and an opaque `next_cursor`; absent `next_cursor` indicates the final page

**Error Scenarios**:
- PDP denies or returns empty constraints → 403 Forbidden (fail closed)
- PDP non-Denied error (network, timeout, internal) → 403 Forbidden (fail-closed)
- 403 response: RFC 9457 `application/problem+json` with `context = {"error": "forbidden"}` — generic message only; MUST NOT include PDP error details, constraint names, or policy names
- Mandatory `from` or `to` parameter missing or malformed → 400 Bad Request
- `cursor` is present but cannot be decoded as a valid (timestamp, id) composite → 400 Bad Request

**Steps**:
1. [x] - `p2` - Consumer: API: GET /usage-collector/v1/raw (?from=, &to=, &cursor=, &page_size=, &usage_type=, &subject_id=, &subject_type=, &resource_id=, &resource_type=) - `inst-raw-1`
2. [x] - `p2` - Gateway: derive tenant_id from SecurityContext - `inst-raw-2`
3. [x] - `p2` - Gateway: validate mandatory parameters (from, to present); validate max-length of string filter fields (usage_type, resource_type, subject_type, source); reject with 400 if any exceeds MAX_FILTER_STRING_LEN; validate page_size ∈ [1, MAX_PAGE_SIZE]; use DEFAULT_PAGE_SIZE when page_size absent; return 400 if page_size out of range; if cursor is supplied, decode it and verify it is a well-formed (timestamp, id) composite; validate cursor.timestamp ∈ [from, to]; return 400 if out of range - `inst-raw-3`
   1. [x] - `p2` - **IF** `from >= to` → **RETURN** 400 validation error (time range must be strictly ascending) - `inst-raw-3a`
   2. [x] - `p2` - **IF** `(to - from) > MAX_QUERY_TIME_RANGE` → **RETURN** 400 validation error (range exceeds configured limit; `MAX_QUERY_TIME_RANGE` defined in §7 OPS-FDESIGN-002) - `inst-raw-3b`
4. [x] - `p2` - **IF** validation fails - `inst-raw-4`
   1. [x] - `p2` - **RETURN** 400 Bad Request as RFC 9457 `application/problem+json`; the `context` extension member is `{"error": "<message>", "code": "VALIDATION_ERROR", "details": ["<field>: <reason>", ...]}` - `inst-raw-4a`
5. [x] - `p2` - Gateway: invoke authorize-and-compile-scope (`cpt-cf-usage-collector-algo-query-api-authz-delegate`) for USAGE_RECORD_READ / LIST - `inst-raw-5`
6. [x] - `p2` - **IF** authorization failed - `inst-raw-6`
   1. [x] - `p2` - **RETURN** 403 Forbidden - `inst-raw-6a`
7. [x] - `p2` - Gateway: build RawQuery (scope from compiled AccessScope, time_range, decoded cursor, page_size; pass user-supplied optional filters from HTTP query parameters: usage_type, resource_id, resource_type, subject_type, subject_id) - `inst-raw-7`
8. [x] - `p2` - Gateway: Plugin: query_raw(RawQuery) → Page<UsageRecord> — the gateway has already compiled the PDP-derived `AccessScope` into `RawQuery.scope` (see `inst-raw-7`); the plugin contract takes no separate `SecurityContext` - `inst-raw-8`
   1. [x] - `p2` - **IF** plugin returns `Err(e)` → **LOG** at `ERROR` level with correlation ID (no PII) → **RETURN** `503 Service Unavailable` as RFC 9457 `application/problem+json` with `context = {"error": "service_unavailable", "correlation_id": "<id>"}` ; the `correlation_id` field MUST match the value logged at `ERROR` level. - `inst-raw-8b`
9. [x] - `p2` - **RETURN** 200 OK with `Page<UsageRecord>` (`items` array + `page_info.next_cursor`; absent `page_info.next_cursor` signals the final page; empty `items` with absent `page_info.next_cursor` is a valid success) - `inst-raw-9`

> **Retry guidance**: 4xx responses (400, 403) are not retryable. 5xx responses (503) should be retried by the caller with exponential backoff; the gateway does not retry internally.

## 3. Processes / Business Logic (CDSL)

### Authorize and Compile Scope

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-query-api-authz-delegate`

**Code location**: implemented in the gateway crate at `usage-collector/src/domain/authz.rs`. The query handlers in `usage-collector/src/api/rest/handlers.rs` invoke this module via `Service` (`usage-collector/src/domain/service.rs`) before delegating to the plugin; the resulting `AccessScope` is embedded into `AggregationQuery.scope` / `RawQuery.scope` so the plugin contract takes no separate `SecurityContext`.

**Input**: `SecurityContext`, resource type constant (`USAGE_RECORD_READ`), action (`actions::LIST`)

**Output**: `AccessScope` on success; `Err(PermissionDenied)` when PDP denies or returns no constraints

**Steps**:
1. [x] - `p1` - Build AccessRequest with require_constraints(true) and BarrierMode::Respect; omit resource_property calls — no specific resource is known at query time - `inst-authz-1`
2. [x] - `p1` - Call PolicyEnforcer::new(authz).access_scope_with(ctx, &USAGE_RECORD_READ, actions::LIST, None, &request) - `inst-authz-2`
3. [x] - `p1` - **IF** PDP returns Err(Denied) — which includes the require_constraints(true) fail-closed path when no constraints are returned - `inst-authz-3`
   1. [x] - `p1` - **RETURN** Err(PermissionDenied); never fall through to a permissive path - `inst-authz-3a`
> **Note (ID encoding)**: This step is logically step 3b (a sub-condition of the PDP Err handling started in step 3); it was renumbered to step 4 in v2.1 to avoid nested numbering. The ID `inst-authz-3b` is intentionally retained for traceability continuity.

4. [x] - `p1` - **IF** PDP returns any other `Err` variant (`NetworkError`, `Timeout`, `InternalError`, or any non-`Denied` error) → **LOG** at `ERROR` level with correlation ID: `"PDP infrastructure error (non-Denied): {variant_name}; correlation_id={id}"` (no PII, no raw error details) → **RETURN** `Err(PermissionDenied)`; fail-closed. No PDP error falls through to data access. - `inst-authz-3b`
5. [x] - `p1` - Compile Vec<Constraint> into AccessScope via compile_to_access_scope(). AccessScope encodes a disjunction of constraint groups (OR-of-ANDs): each Constraint becomes one AND-group. All fields within a single Constraint are combined with AND (conjunction); AccessScope is a disjunction (OR) of these AND-groups. compile_to_access_scope() preserves this structure without flattening. OR-of-ANDs constraint structure must be preserved exactly — flattening multiple constraints to independent AND conditions is a security violation. Flattening violates cpt-cf-usage-collector-principle-fail-closed (see `cpt-cf-usage-collector-constraint-or-of-ands-preservation`). - `inst-authz-4`
6. [x] - `p1` - **RETURN** AccessScope - `inst-authz-5`

### SDK Type Additions

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-query-api-sdk-types`

**Input**: None (compile-time additions to `usage-collector-sdk`)

**Output**: New types and trait operations exported from `usage-collector-sdk`

**Steps**:
0. [x] - `p1` - **Note**: DateTime parameters (from, to in flows and time_range in queries): RFC 3339 UTC only. Offset-aware datetimes MUST be rejected with 400 Bad Request. page_size: when absent in request, default to DEFAULT_PAGE_SIZE. - `inst-sdk-0`
1. [x] - `p1` - Add `AggregationFn` enum to SDK models: variants Sum, Count, Min, Max, Avg - `inst-sdk-1`
2. [x] - `p1` - Add `BucketSize` type representing a time granularity for TimeBucket grouping - `inst-sdk-2`
3. [x] - `p1` - Add `GroupByDimension` enum: variants TimeBucket(BucketSize), UsageType, Subject, Resource, Source - `inst-sdk-3`
4. [x] - `p1` - Add `AggregationQuery` struct: fields scope: AccessScope, time_range: (DateTime<Utc>, DateTime<Utc>), function: AggregationFn, group_by: Vec<GroupByDimension>, bucket_size: Option<BucketSize>, usage_type: Option<String>, resource_id: Option<Uuid>, resource_type: Option<String>, subject_id: Option<Uuid>, subject_type: Option<String>, source: Option<String> - `inst-sdk-4`
   > **Parameterization**: String filter fields (usage_type, resource_type, subject_type, source) MUST be passed as parameterized storage query arguments; string interpolation into query strings is prohibited.
   >
   > **Filter composition**: All present optional filters AND the AccessScope scope are applied conjunctively (AND semantics).
5. [x] - `p1` - Add `AggregationResult` struct: fields function: AggregationFn, value: f64, bucket_start: Option<DateTime<Utc>>, usage_type: Option<String>, subject_id: Option<Uuid>, subject_type: Option<String>, resource_id: Option<Uuid>, resource_type: Option<String>, source: Option<String>; each Option field is populated only when the corresponding GroupByDimension was requested. Option fields in AggregationResult are absent (not null) in JSON serialization when the corresponding GroupByDimension was not requested. - `inst-sdk-5`
6. [x] - `p2` - Use `CursorV1` from `modkit-odata` as the cursor type and `Page<T>` from `modkit-odata` as the paginated result type (`Page<T>` fields: `items: Vec<T>`, `page_info: PageInfo`; `PageInfo` fields: `next_cursor: Option<String>`, `prev_cursor: Option<String>`, `limit: u64`). `CursorV1` encodes an exclusive lower-bound keyset cursor (timestamp + id). Plugin MUST return records ordered ascending by (timestamp, id) WHERE (timestamp, id) > cursor position within the requested time range. A `CursorV1` from a different [from, to] range SHOULD be rejected with 400 Bad Request. Cursor stability: a `CursorV1` is valid for the lifetime of the request that produced it. Concurrent data writes SHOULD NOT invalidate an in-progress pagination sequence; implementations MAY use snapshot isolation or equivalent to guarantee stability within a single paginated traversal. Retry idempotency: repeating a raw query with the same `CursorV1` returns the same page when the underlying data is unchanged. Delete consistency: records deleted between page fetches MAY cause a page to contain fewer items than `page_size`; the cursor still advances to the next position after the last returned record; callers MUST tolerate short pages. - `inst-sdk-6`
   > **Cursor encoding**: `CursorV1` payload is base64url-encoded JSON opaque state from `modkit-odata`. No HMAC signing is applied in this feature (tamper-resistance deferred to a future feature — see Known Limitations). Format is versioned (`"v":1`); format changes require incrementing the version field.
7. [x] - `p2` - Add `RawQuery` struct: fields scope: AccessScope, time_range: (DateTime<Utc>, DateTime<Utc>), usage_type: Option<String>, resource_id: Option<Uuid>, resource_type: Option<String>, subject_type: Option<String>, subject_id: Option<Uuid>, cursor: Option<CursorV1>, page_size: usize - `inst-sdk-7`
   > **Bounds**: page_size MUST be validated in [1, MAX_PAGE_SIZE]; absent page_size defaults to DEFAULT_PAGE_SIZE. MAX_PAGE_SIZE and DEFAULT_PAGE_SIZE are gateway configuration constants (see §7 OPS-FDESIGN-002).
   >
   > **Filter composition**: All present optional filters AND the AccessScope scope are applied conjunctively (AND semantics).
   >
   > **Feature flags**: Not applicable for this feature; see §7.
8. [x] - `p1` - Add `query_aggregated(&self, query: AggregationQuery) -> Result<Vec<AggregationResult>, UsageCollectorError>` to `UsageCollectorPluginClientV1` — the gateway compiles the PDP-derived `AccessScope` and embeds it in `AggregationQuery.scope` (see `inst-agg-7`), so the plugin contract takes no separate `SecurityContext`; breaking trait change; all existing implementations must be updated - `inst-sdk-8`
9. [x] - `p2` - Add `query_raw(&self, query: RawQuery) -> Result<Page<UsageRecord>, UsageCollectorError>` to `UsageCollectorPluginClientV1` — the gateway compiles the PDP-derived `AccessScope` and embeds it in `RawQuery.scope` (see `inst-raw-7`), so the plugin contract takes no separate `SecurityContext`; breaking trait change; all existing implementations must be updated - `inst-sdk-9`

### Noop Plugin Query Stubs

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-query-api-noop-stubs`

**Input**: New trait operations on `UsageCollectorPluginClientV1`

**Output**: Compiling, non-panicking stub implementations in `noop-usage-collector-storage-plugin`

**Steps**:
1. [x] - `p1` - Implement `query_aggregated` on `NoopUsageCollectorStoragePlugin`: accept the query, ignore it, return Ok(vec![]) — no storage access, no error - `inst-noop-1`
2. [x] - `p2` - Implement `query_raw` on `NoopUsageCollectorStoragePlugin`: accept the query, ignore it, return `Ok(Page::new(vec![], PageInfo { next_cursor: None, prev_cursor: None, limit: query.page_size as u64 }))` — no storage access, no error - `inst-noop-2`

### Plugin Trait Contract Requirements

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-algo-query-api-plugin-contract`

**Input**: `AccessScope, query parameters`

**Output**: `plugin-specific result set`

**Steps**:
1. [ ] - `p2` - String filter fields MUST be parameterized in all storage queries; direct string interpolation is prohibited. - `inst-plugin-contract-1`
2. [ ] - `p2` - AccessScope constraints MUST be applied as mandatory row filters; each Constraint is an AND-group (all fields within one Constraint must match simultaneously); constraints across groups are combined with OR. - `inst-plugin-contract-2`
3. [x] - `p2` - query_aggregated MUST return at most MAX_AGG_ROWS rows; if the result set would exceed this limit, the plugin MUST return `Err(UsageCollectorError::ResourceExhausted)` (built via `UsageRecordError::resource_exhausted("query result too large")`) instead of silently truncating. - `inst-plugin-contract-3`
4. [ ] - `p2` - Plugins MUST use the configured connection pool; opening bare connections is prohibited. - `inst-plugin-contract-4`
5. [ ] - `p2` - Both query_aggregated and query_raw are read-only; plugins MUST NOT open write transactions for these operations. - `inst-plugin-contract-5`

## 4. States (CDSL)

Not applicable. Query operations are stateless reads within a single request scope. No entity lifecycle transitions are introduced by this feature. `UsageRecord` status transitions (`active` → `inactive`) belong to Feature 8 (Operator Operations).

## 5. Definitions of Done

### SDK Types and Trait Operations

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-query-api-sdk-types`

The system MUST add `AggregationFn`, `GroupByDimension`, `BucketSize`, `AggregationQuery`, `AggregationResult`, and `RawQuery` to `usage-collector-sdk`, use `CursorV1` and `Page<T>` from `modkit-odata` as the cursor and paginated result types, and MUST add `query_aggregated` and `query_raw` operations to `UsageCollectorPluginClientV1`.

**Implements**:
- `cpt-cf-usage-collector-algo-query-api-sdk-types`

**Constraints**: `cpt-cf-usage-collector-constraint-security-context`

**Touches**:
- Entities: `AggregationQuery`, `AggregationResult`, `RawQuery`, `CursorV1` (modkit-odata), `Page<T>` (modkit-odata), `PageInfo` (modkit-odata)
- Crate: `usage-collector-sdk`

### Noop Plugin Query Stubs

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-query-api-noop-stubs`

The system MUST implement `query_aggregated` and `query_raw` on `NoopUsageCollectorStoragePlugin`, returning empty results without error, so the noop plugin satisfies the full `UsageCollectorPluginClientV1` trait after the breaking trait change.

**Implements**:
- `cpt-cf-usage-collector-algo-query-api-noop-stubs`

**Touches**:
- Crate: `noop-usage-collector-storage-plugin`

### Gateway Aggregated Query Handler

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-query-api-gateway-aggregated`

The system MUST expose `GET /usage-collector/v1/aggregated` that derives tenant from SecurityContext (never from query parameters), validates mandatory parameters (`fn`, `from`, `to`; `bucket_size` required when `group_by` includes `time_bucket`), authorizes via the platform PDP with `require_constraints(true)` returning 403 when PDP denies or returns empty constraints, builds an `AggregationQuery` with the compiled `AccessScope` and user-supplied optional filters, delegates to the active storage plugin via `query_aggregated`, and returns 200 with the result array. The endpoint MUST be registered via `OperationBuilder::get(...).authenticated().require_license_features::<License>([])` against the platform license feature `gts.cf.core.lic.feat.v1~cf.core.global.base.v1`; the gateway returns 403 to callers whose tenant is not licensed for this feature. The OpenAPI registry MUST declare 400 (`error_400`), 403 (`error_403`), 500 (`error_500`), and 503 (`problem_response(SERVICE_UNAVAILABLE, "Service Unavailable")`); the runtime 503 emission per `inst-agg-8c` is produced by the canonical Problem mapper from `Err(UsageCollectorError::ServiceUnavailable)` at the handler boundary. 403 response context is `{"error": "forbidden"}` — generic message only; MUST NOT include PDP error details, constraint names, policy names, or role names.

**Implements**:
- `cpt-cf-usage-collector-flow-query-api-aggregated`
- `cpt-cf-usage-collector-algo-query-api-authz-delegate`

**Sequences**: `cpt-cf-usage-collector-seq-query-aggregated`

**Constraints**: `cpt-cf-usage-collector-constraint-security-context`, `cpt-cf-usage-collector-constraint-no-business-logic`, `cpt-cf-usage-collector-constraint-or-of-ands-preservation`

**Touches**:
- API: `GET /usage-collector/v1/aggregated`
- Crate: `usage-collector` (gateway)

### Gateway Raw Query Handler

- [x] `p2` - **ID**: `cpt-cf-usage-collector-dod-query-api-gateway-raw`

The system MUST expose `GET /usage-collector/v1/raw` that derives tenant from SecurityContext, validates mandatory parameters and decodes any supplied cursor, authorizes via the platform PDP with `require_constraints(true)` returning 403 when PDP denies or returns empty constraints, builds a `RawQuery` with the compiled `AccessScope` plus cursor and optional filters, delegates to the active storage plugin via `query_raw`, and returns 200 with the `Page<UsageRecord>`. The endpoint MUST be registered via `OperationBuilder::get(...).authenticated().require_license_features::<License>([])` against the platform license feature `gts.cf.core.lic.feat.v1~cf.core.global.base.v1`; the gateway returns 403 to callers whose tenant is not licensed for this feature. The OpenAPI registry MUST declare 400 (`error_400`), 403 (`error_403`), 500 (`error_500`), and 503 (`problem_response(SERVICE_UNAVAILABLE, "Service Unavailable")`); the runtime 503 emission per `inst-raw-8b` is produced by the canonical Problem mapper from `Err(UsageCollectorError::ServiceUnavailable)` at the handler boundary. 403 response context is `{"error": "forbidden"}` — generic message only; MUST NOT include PDP error details, constraint names, policy names, or role names.

**Implements**:
- `cpt-cf-usage-collector-flow-query-api-raw`
- `cpt-cf-usage-collector-algo-query-api-authz-delegate`

**Sequences**: `cpt-cf-usage-collector-seq-query-raw`

**Constraints**: `cpt-cf-usage-collector-constraint-security-context`, `cpt-cf-usage-collector-constraint-no-business-logic`, `cpt-cf-usage-collector-constraint-or-of-ands-preservation`

**Touches**:
- API: `GET /usage-collector/v1/raw`
- Crate: `usage-collector` (gateway)

### Plugin Trait Contract

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-query-api-plugin-contract`

All storage plugin implementations of `query_aggregated` and `query_raw` MUST: (1) parameterize all string filter fields in storage queries — direct interpolation is prohibited; (2) apply `AccessScope` constraints as mandatory row filters — omitting them is a security violation; (3) return at most MAX_AGG_ROWS rows from `query_aggregated` — if the result set would exceed this limit, the plugin MUST return `Err(UsageCollectorError::ResourceExhausted)` (built via `UsageRecordError::resource_exhausted("query result too large")`); (4) use the configured connection pool — bare connections are prohibited; (5) treat both operations as read-only — write transactions are prohibited.

**Implements**:
- `cpt-cf-usage-collector-algo-query-api-plugin-contract`

**Constraints**: `cpt-cf-usage-collector-constraint-security-context`, `cpt-cf-usage-collector-constraint-or-of-ands-preservation`

## 6. Acceptance Criteria

- [ ] `GET /usage-collector/v1/aggregated` with a valid SecurityContext and mandatory parameters returns 200 with a (possibly empty) array of aggregation result rows
- [ ] `GET /usage-collector/v1/aggregated` with `group_by=time_bucket` but no `bucket_size` returns 400
- [ ] `GET /usage-collector/v1/aggregated` when the PDP denies or returns no constraints returns 403
- [ ] `GET /usage-collector/v1/aggregated` does not accept `tenant_id` as a query parameter; tenant is always derived from SecurityContext
- [ ] `GET /usage-collector/v1/raw` with a valid SecurityContext, `from`, and `to` returns 200 with a `Page<UsageRecord>`
- [ ] `GET /usage-collector/v1/raw` with a malformed cursor returns 400
- [ ] `GET /usage-collector/v1/raw` when the PDP denies or returns no constraints returns 403
- [ ] `GET /usage-collector/v1/raw` response with absent `page_info.next_cursor` indicates the final page; empty items with absent `page_info.next_cursor` is a valid success
- [ ] Noop plugin `query_aggregated` returns `Ok(vec![])` without error
- [ ] Noop plugin `query_raw` returns `Ok(Page::new(vec![], PageInfo { next_cursor: None, prev_cursor: None, limit: ... }))` without error
- [ ] `UsageCollectorPluginClientV1` compiles with the new `query_aggregated` and `query_raw` operations
- [ ] `GET /usage-collector/v1/raw` with `page_size=0` returns 400 Bad Request
- [ ] `GET /usage-collector/v1/raw` with `page_size > MAX_PAGE_SIZE` returns 400 Bad Request
- [ ] `GET /usage-collector/v1/raw` with absent `page_size` uses `DEFAULT_PAGE_SIZE`
- [ ] `GET /usage-collector/v1/raw` with cursor whose timestamp is outside [from, to] returns 400 Bad Request
- [ ] `GET /usage-collector/v1/aggregated` when plugin returns `Err(ResourceExhausted)` (canonical carrier for `MAX_AGG_ROWS` exceeded; built via `UsageRecordError::resource_exhausted("query result too large")`) is mapped via the canonical Problem flow per `inst-agg-8c` — there is no gateway-side 400 'query too broad' shortcut
- [ ] `GET /usage-collector/v1/aggregated` when plugin returns `Err` returns 503 Service Unavailable
- [ ] `GET /usage-collector/v1/raw` when plugin returns `Err` returns 503 Service Unavailable
- [ ] `GET /usage-collector/v1/aggregated` and `GET /usage-collector/v1/raw` when PDP returns non-Denied error return 403 (fail-closed)
- [ ] `GET /usage-collector/v1/aggregated` and `GET /usage-collector/v1/raw` with a string filter field exceeding `MAX_FILTER_STRING_LEN` return 400 Bad Request
- [ ] `GET /usage-collector/v1/aggregated` and `GET /usage-collector/v1/raw` are registered with `OperationBuilder::require_license_features::<License>([])` against the platform license feature `gts.cf.core.lic.feat.v1~cf.core.global.base.v1`; calls from a tenant whose license does not carry this feature receive 403 from the license gate (before the PDP authorize step is reached)
- [ ] The OpenAPI document generated from the gateway's route registry exposes `error_400` / `error_403` / `error_500` and the `503` `problem_response` for both query routes, so generated clients can handle every status the handler emits.

### Performance Acceptance Criteria
- [ ] `GET /usage-collector/v1/aggregated` p95 latency ≤ 500ms for time ranges up to 30 days at production record volumes (`cpt-cf-usage-collector-nfr-query-latency`) (verification deferred to Feature 4 — see PERF-FDESIGN-004)
- [ ] `GET /usage-collector/v1/raw` p95 latency ≤ 500ms for page_size ≤ DEFAULT_PAGE_SIZE at production record volumes (`cpt-cf-usage-collector-nfr-query-latency`) (verification deferred to Feature 4 — see PERF-FDESIGN-004)
- [ ] Throughput and resource baseline metrics: deferred to Feature 4 (see §7 PERF-FDESIGN-004)
- [ ] Multi-page cursor traversal returns all records without duplicates or gaps across pages
- [ ] AccessScope filtering returns only records matching the caller's scope; records outside scope are never returned

## 7. Non-Applicability Notes

- **SEC-FDESIGN-001 (Authentication Integration)**: Not applicable. All gateway endpoints are protected by ModKit request-pipeline authentication, which rejects unauthenticated requests at the framework level before any handler is invoked. This feature introduces no authentication logic — authentication is a transparent infrastructure guarantee.

- **Section 4 (States)**: Not applicable. Query operations are stateless reads; no entity lifecycle is introduced by this feature. `UsageRecord` status transitions are owned by Feature 8 (Operator Operations).

- **SEC-FDESIGN-005 (Audit Trail)**: Not applicable. Both query endpoints are read-only; no data is mutated. Audit event emission (`WriteAuditEvent` to `audit_service`) applies only to operator-initiated write operations (backfill, amendment, deactivation) covered by Feature 8.

- **OPS-FDESIGN-001 (Observability)**: The new query endpoints inherit the gateway's general ModKit observability infrastructure — structured request logging, distributed tracing via `SecurityContext` correlation, and the existing plugin call metrics. No query-specific metrics are introduced by this feature; per-query latency and throughput tracking are deferred to Feature 4 (Production Storage Plugin) where storage-level instrumentation is appropriate.

- **REL-FDESIGN-002 (Fault Tolerance — gateway timeout and circuit breaker)**: The gateway's configurable 5 s per-call timeout and circuit breaker (opens after 5 consecutive failures within a 10 s window; half-open probe after 30 s) defined in DESIGN §3.2 (`cpt-cf-usage-collector-component-gateway`) apply equally to `query_aggregated` and `query_raw` plugin calls. These behaviours are owned by the gateway component specification and are not redefined here. PDP call fault tolerance: subject to gateway 5 s per-call timeout; on PDP timeout or network error, authorize-and-compile-scope returns Err(PermissionDenied) — fail-closed. No circuit breaker on PDP calls; no retry.

- **REL-FDESIGN-003 (Data Integrity / Transactions)**: Read-side cursor stability: `inst-sdk-6` defines `CursorV1` (from `modkit-odata`) as an exclusive lower-bound keyset cursor (timestamp, id). Concurrent writes after cursor issuance SHOULD NOT invalidate an in-progress traversal; implementations MAY use snapshot isolation. Retrying a raw query with the same `CursorV1` returns the same page when the underlying data is unchanged. No write-path transaction boundaries, rollback scenarios, or cross-request idempotency requirements are introduced by this feature.

- **COMPL (Compliance & Regulatory)**:
  - *Applicable frameworks*: GDPR, SOC 2 Type II (platform compliance posture — see DATA-FDESIGN-005 and SEC-FDESIGN-004).
  - *Data subject rights (GDPR Art. 15–22 — access, erasure, portability)*: Not applicable to this feature. This feature introduces read-only query endpoints; no new data collection, processing purpose, or retention decision is made. Data subject right operations (erasure, export) are Feature 8 (Operator Operations) scope.
  - *Consent and lawful basis*: Not applicable. Usage data is collected for platform operational purposes under legitimate interest; no new consent mechanism is introduced by this feature.
  - *Data minimisation and purpose limitation*: `AccessScope` constraint enforcement (inst-authz-4) restricts query results to the caller's authorised scope — this is the platform's data minimisation mechanism for queries (see DATA-FDESIGN-005).
  - *Audit logging of data access*: Not applicable for read-only query endpoints at this time; audit trail for write operations is Feature 8. PDP-level access decisions are logged (inst-authz-3b) with no PII in log entries (SEC-FDESIGN-004).
  - *UI/UX compliance (cookie banners, privacy notices)*: Not applicable. Backend API feature; no user-facing UI.
  - *Cross-border data transfer*: Not applicable. Data residency is a platform deployment concern; this feature introduces no new data transfer paths.

- **DATA-FDESIGN-005 (Data Classification and Transmission)**: `subject_id` and `resource_id` are sensitive identifiers returned in query results. Transmission: platform-level TLS (inherited). Data minimization: `AccessScope` governs which records are accessible; no field-level masking is applied at the gateway layer. `subject_id` and `resource_id` MUST NOT appear in error log messages or structured error responses.

- **OPS-FDESIGN-004 (Breaking Change Rollout)**: Rollout: single atomic PR containing the SDK crate change (new trait operations), noop plugin stubs, and gateway handler additions. Rollback: revert PR atomically; no data migration required. Backward compatibility: not maintained — `query_aggregated` and `query_raw` additions to `UsageCollectorPluginClientV1` are compile-time breaking changes within the workspace.

- **SEC-FDESIGN-004 (PII Classification and Transmission Security)**: `subject_id` and `resource_id` in query results are sensitive identifiers. Classification: internal-sensitive (platform-level identifiers). Transmission security: platform-level TLS. These fields MUST NOT appear in error responses, log messages, or audit-trail entries that could be observed by unauthorized parties. See also DATA-FDESIGN-005.

- **REL-FDESIGN-004 (Resilience Patterns)**: Queue overflow and deadlock prevention: not applicable (synchronous request-response; no queues). Backpressure: inherited from ModKit HTTP layer. Resource exhaustion prevention: `page_size` bounded to [1, MAX_PAGE_SIZE]; aggregation result set capped at MAX_AGG_ROWS. Bulkhead: separate handler paths per `cpt-cf-usage-collector-nfr-workload-isolation`.

- **REL-FDESIGN-005 (Recovery Procedures)**: Not applicable. Both query endpoints are stateless read operations; no partial state is created on failure. No reconciliation or compensating transactions are required. Recovery from 5xx errors: client-side retry with exponential backoff.

- **Feature Flags**: Not applicable. Both query endpoints are new additions to an existing module; no conditional feature-flag enablement is required.

- **SEC-FDESIGN-006 (403 / 503 Response Envelope)**: Both endpoints return error responses as RFC 9457 `application/problem+json` documents. The application-specific envelope sits in the `context` extension member (not at the top level): `context = {"error": "forbidden"}` on 403 and `context = {"error": "service_unavailable", "correlation_id": "<id>"}` on 503. The 403 `context` MUST NOT include PDP error details, constraint names, policy names, or role names. Caller guidance: treat 403 as opaque access denial. The 503 `correlation_id` is a fresh per-request UUID (not derived from `SecurityContext.subject_id`, which would collapse every 503 from one caller into a single id); it MUST NOT contain PII, stack traces, or plugin error details.

- **SEC (XSS / Cross-Site Scripting)**: Not applicable. This feature exposes JSON-only REST endpoints with no HTML rendering, no template engine, no JavaScript execution context, and no reflection of user input into HTML. JSON responses are serialized by Rust's serde_json; no content is inserted into HTML documents.

- **SEC (Command Injection)**: Not applicable. This feature introduces no shell command execution, process spawning, or operating-system-level calls triggered by user input. All storage delegation is via the Rust trait interface (`UsageCollectorPluginClientV1`); no string interpolation into shell or OS commands occurs.

- **SEC (Path Traversal)**: Not applicable. This feature introduces no file system access, file path construction from user input, or directory traversal logic. Query parameters are used only as typed Rust structs passed to storage plugin methods.

- **PERF-FDESIGN-001 (Query Latency and Caching)**: Hot paths: GET /aggregated (PDP call → query_aggregated → serialize) and GET /raw (PDP call → query_raw → serialize page). No result caching at the gateway layer. N+1 prevention: exactly one PDP call and one plugin call per request. Query latency targets: p95 ≤ 500ms for aggregated (30-day range) and raw (page_size ≤ DEFAULT_PAGE_SIZE) queries at production record volumes (`cpt-cf-usage-collector-nfr-query-latency`); see §6 Performance Acceptance Criteria.

- **PERF-FDESIGN-002 (Memory Allocation & Resource Cleanup)**:
  - *Memory allocation*: Bounded by `MAX_AGG_ROWS` × sizeof(AggregationResult) for aggregated responses and `MAX_PAGE_SIZE` × sizeof(UsageRecord) for raw responses. Both constants are gateway configuration (see OPS-FDESIGN-002). No unbounded heap allocation.
  - *Resource cleanup*: Not applicable; handled by Rust RAII. No manual memory management, connection leak risk, or deferred cleanup. Plugin connections are managed by the configured connection pool (plugin-contract item 4).
  - *Streaming*: Not applicable. Both endpoints return fully-buffered JSON responses (bounded by the above limits). Streaming is not used and not required for the expected response sizes at this feature's scope.

- **PERF-FDESIGN-004 (Performance Testing & Baselines)**:
  - *Throughput targets*: Not established for this feature; query throughput is constrained by storage plugin capacity, which is outside this feature's scope. Throughput target definition deferred to Feature 4 (Production Storage Plugin) when a concrete backend is available for benchmarking.
  - *Resource usage limits*: Not applicable at the gateway layer for this feature. Per-request memory is bounded by `MAX_AGG_ROWS` × sizeof(AggregationResult) for aggregated queries and `MAX_PAGE_SIZE` × sizeof(UsageRecord) for raw queries; both are gateway configuration constants (see OPS-FDESIGN-002). No heap allocation beyond bounded response buffers.
  - *Performance test requirement*: Deferred to Feature 4 alongside E2E tests (see TEST-FDESIGN-002). Unit and integration tests with the noop plugin cannot exercise storage performance. Performance test infrastructure requires a production-grade storage backend.
  - *Baseline metrics*: Not applicable at this stage; baseline cannot be established without a production storage backend. Defer to Feature 4.

- **INT-FDESIGN-002 (Database Integration)**:
  - *Index and join patterns*: Not applicable at the gateway layer. All database operations are delegated entirely to the active storage plugin via `query_aggregated` and `query_raw` trait operations. The gateway has no direct database connection and imposes no schema, index, or query structure on the storage backend. Index and join strategies are a Feature 4 (Production Storage Plugin) concern.
  - *Connection management*: Covered by plugin-contract item 4 — plugins MUST use the configured connection pool; no gateway-layer connection management is introduced.
  - *ORM / query builder*: Not applicable at this layer; see Feature 4.

- **INT-FDESIGN-003 (PDP Integration)**: PolicyEnforcer (PDP) is called once per request. Timeout: 5 s. Failure mode: any non-Ok result (including NetworkError, Timeout, InternalError) → Err(PermissionDenied) — fail-closed (see authorize-and-compile-scope, inst-authz-3b). No retry; no circuit breaker on PDP calls. Storage plugin timeout and circuit breaker: DESIGN §3.2 (cpt-cf-usage-collector-component-gateway).
  - *PDP client auth*: requests to PDP use the auth mechanism provided by the platform PolicyEnforcer abstraction (not yet specified at this layer; authentication between the gateway and PDP is a platform infrastructure concern resolved during deployment configuration).
  - *Version compatibility*: PDP API version not yet specified at this feature's scope; the gateway delegates entirely via `PolicyEnforcer::access_scope_with` — breaking changes to the PDP API version require re-validation of the `authorize-and-compile-scope` algorithm and `AccessScope` compilation.

- **OPS-FDESIGN-002 (Configuration Parameters)**: Introduces five gateway configuration constants, all environment-configurable via ModKit. Startup validation: `DEFAULT_PAGE_SIZE ≤ MAX_PAGE_SIZE` and both > 0; `MAX_FILTER_STRING_LEN > 0`.

  | Constant | Type | Description |
  |---|---|---|
  | `DEFAULT_PAGE_SIZE` | integer | Default number of records returned per page when `page_size` is absent. Recommended: 100. |
  | `MAX_PAGE_SIZE` | integer | Maximum allowed value for `page_size`. Recommended: 1 000. |
  | `MAX_AGG_ROWS` | integer | Maximum number of rows that `query_aggregated` may return; exceeding this causes `UsageCollectorError::ResourceExhausted` (built via `UsageRecordError::resource_exhausted("query result too large")`). Recommended: 10 000. |
  | `MAX_FILTER_STRING_LEN` | integer | Maximum byte length for string filter fields (`usage_type`, `resource_type`, `subject_type`, `source`). Recommended: 256. |

- **OPS-FDESIGN-003 (Health and Diagnostics)**: No new health check endpoints. The existing gateway health endpoint reflects circuit breaker state for all plugin calls including the new query operations (DESIGN §3.2). Troubleshooting: persistent 403 responses → PDP unavailability or missing grant; persistent 5xx responses → plugin circuit breaker in open state.

- **TEST-FDESIGN-001 (Unit Test Strategy)**: Unit tests are required for: (1) SDK type construction and JSON round-trip for `AggregationQuery`, `AggregationResult`, `RawQuery`, `Page<T>`, and `CursorV1`; (2) `CursorV1` encode/decode round-trip — valid composite, malformed input returns error; (3) authorize-and-compile-scope — Err(Denied) path, non-Denied PDP error path, single-constraint AccessScope, multi-constraint AccessScope (OR-of-ANDs preserved); (4) parameter validation boundary conditions — from ≥ to, range > MAX_QUERY_TIME_RANGE, page_size = 0, page_size > MAX_PAGE_SIZE, absent page_size defaults to DEFAULT_PAGE_SIZE, string filter field at MAX_FILTER_STRING_LEN (pass) and MAX_FILTER_STRING_LEN + 1 (fail). Mocking strategy: PDP (PolicyEnforcer) and storage plugin are both mocked at the trait boundary for unit tests.

- **TEST-FDESIGN-002 (Test Coverage)**: Unit tests: authorize-and-compile-scope — anonymous principal guard, Err(Denied), non-Denied PDP error, single constraint, multiple constraints (OR-of-ANDs preserved). Unit tests: parameter validation — missing mandatory params, malformed cursor, page_size=0, page_size>MAX_PAGE_SIZE. Integration tests: gateway handler ↔ noop plugin (200/400/403/503 paths). E2E tests: deferred to Feature 4 (production storage backend).

- **DATA-FDESIGN-001 (Data Access Patterns — index and join strategies)**:
  - Deferred to Feature 4 (Production Storage Plugin). This feature introduces no data access patterns at the gateway layer; all query execution is delegated to the plugin. The gateway specifies query parameters (AggregationQuery, RawQuery) but not execution strategy. Index selection, join patterns, and query optimisation are the storage plugin's responsibility. See also §7 OPS-FDESIGN-001 and TEST-FDESIGN-002 for similar Feature 4 deferrals.

- **DATA-FDESIGN-004 (Data Lifecycle)**: Not applicable. Both query endpoints are read-only; no data is created, updated, deleted, or archived by this feature. Data creation: Feature 1 (Ingest Pipeline). Deletion and archival: Feature 8 (Operator Operations).

- **PERF-FDESIGN-003 (Scalability and Rate Limiting)**: Both operations are stateless (§4); horizontal scaling is provided by the platform deployment layer. Rate limiting: inherited from ModKit HTTP layer. Throttling: circuit breaker for plugin calls (DESIGN §3.2).

- **INT-FDESIGN-004 (Event and Message Handling)**: Not applicable. Both query endpoints are read-only; no events are published or consumed. Audit event emission for write operations: Feature 8 (Operator Operations).

- **INT-FDESIGN-005 (Cache Integration)**: Not applicable. No result caching at the gateway layer (see PERF-FDESIGN-001). Caching strategies at the storage layer: Feature 4 (Production Storage Plugin).

- **UX-FDESIGN-002 (API Ergonomics / Developer Experience)**: This feature introduces backend JSON REST endpoints with no UI component. API ergonomics are addressed as follows: (1) all non-2xx responses are RFC 9457 `application/problem+json` documents; the application-specific envelope is the `context` extension member (e.g. `context = {"error":"validation failed", "code":"VALIDATION_ERROR", "details":[..]}` on 400 — see inst-agg-4a and inst-raw-4a); (2) 403 responses return a generic `context = {"error": "forbidden"}` to avoid leaking policy structure; (3) 503 responses carry a `correlation_id` in `context` matching the server-side log entry to enable support tracing; (4) all date/time parameters use RFC 3339 UTC format (inst-sdk-0); (5) pagination follows the `next_cursor` absent-means-last-page convention with explicit empty-page success semantics. No graphical UX concerns apply.

- **MAINT-FDESIGN-001 (Code Maintainability)**: The feature is decomposed into three isolated, independently compilable units: (1) `usage-collector-sdk` — all public types and trait operations; (2) `noop-usage-collector-storage-plugin` — trivial trait stubs with no business logic; (3) `usage-collector` gateway crate — handler logic only, delegating to the plugin via the trait interface. The `authorize-and-compile-scope` algorithm is extracted as a named, reusable process (`cpt-cf-usage-collector-algo-query-api-authz-delegate`) with explicit Input/Output contracts, enabling independent testing. No shared mutable state is introduced; all handler state flows through `SecurityContext` and typed query structs.

- **MAINT-FDESIGN-002 (API Usage Examples)**:

  *Aggregation query — sum CPU usage by day for January 2026:*

  ```http
  GET /usage-collector/v1/aggregated?fn=sum&from=2026-01-01T00%3A00%3A00Z&to=2026-02-01T00%3A00%3A00Z&group_by%5B%5D=time_bucket&bucket_size=day&usage_type=compute.cpu HTTP/1.1
  Authorization: Bearer <token>
  ```

  Response `200 OK`:

  ```json
  [
    {
      "function": "sum",
      "value": 43200.0,
      "bucket_start": "2026-01-01T00:00:00Z",
      "usage_type": "compute.cpu"
    },
    {
      "function": "sum",
      "value": 38400.0,
      "bucket_start": "2026-01-02T00:00:00Z",
      "usage_type": "compute.cpu"
    }
  ]
  ```

  *Raw query — first page of CPU usage records for January 2026:*

  ```http
  GET /usage-collector/v1/raw?from=2026-01-01T00%3A00%3A00Z&to=2026-02-01T00%3A00%3A00Z&usage_type=compute.cpu&page_size=2 HTTP/1.1
  Authorization: Bearer <token>
  ```

  Response `200 OK`:

  ```json
  {
    "items": [
      {
        "module": "compute",
        "tenant_id": "550e8400-e29b-41d4-a716-446655440000",
        "metric": "compute.cpu",
        "kind": "gauge",
        "value": 1.0,
        "resource_id": "a3f5d890-12c4-4b8a-9a6e-1234567890ab",
        "resource_type": "vcpu",
        "subject_type": "vm",
        "idempotency_key": "vm-cpu-2026-01-01T06:00:00Z-001",
        "timestamp": "2026-01-01T06:00:00Z"
      }
    ],
    "page_info": {
      "next_cursor": "eyJ2IjoxLCJrIjpbIjIwMjYtMDEtMDFUMDY6MDA6MDBaIiwiNTUwZTg0MDAtZTI5Yi00MWQ0LWE3MTYtNDQ2NjU1NDQwMDAwIl0sIm8iOiJhc2MiLCJzIjoiK3RpbWVzdGFtcCwraWQiLCJkIjoiZndkIn0",
      "limit": 2
    }
  }
  ```

  Absent `page_info.next_cursor` in the response indicates the final page. Empty `items` with absent `page_info.next_cursor` is a valid success.

- **MAINT-FDESIGN-003 (Known Limitations)**:
  1. *Cursor tamper-resistance*: No HMAC signing is applied to cursor tokens in this feature. A malicious caller could craft an arbitrary cursor payload. Tamper-resistance via HMAC signing is deferred to a future feature.
  2. *API stability*: The query API (`GET /usage-collector/v1/aggregated`, `GET /usage-collector/v1/raw`) carries SHOULD-level stability at this stage. Stable guarantees are not provided until v1.0 of the usage-collector module. Breaking changes to query parameters or response shapes require a version increment in the API path.
  3. *Cursor TTL not enforced*: `CursorV1` carries no `issued_at` timestamp, so server-side cursor expiry (410 Gone) cannot be implemented with the current type. Callers SHOULD treat cursors as short-lived and not store them beyond a single pagination session. Cursor TTL enforcement is deferred to a future feature that either extends `CursorV1` or wraps it with an issued-at field.

- **MAINT-FDESIGN-002-02 (resolved — OpenAPI 503 declaration)**: The query routes (`GET /usage-collector/v1/aggregated`, `GET /usage-collector/v1/raw`) emit `503 Service Unavailable` at runtime via the canonical Problem mapper when the storage plugin returns `Err(UsageCollectorError::ServiceUnavailable)` (per `inst-agg-8c` / `inst-raw-8b`). The route registration (`usage-collector/src/api/rest/routes.rs`) declares a `problem_response(SERVICE_UNAVAILABLE, "Service Unavailable")` alongside `error_400` / `error_403` / `error_500` for both query routes, so generated OpenAPI clients see the full set of statuses the handler can emit.

