# Feature: Usage Query

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Explicit Non-Applicability](#15-explicit-non-applicability)
  - [1.6 Implementation Status](#16-implementation-status)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Query Aggregated](#query-aggregated)
  - [Query Raw](#query-raw)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Attribution & PDP Authorization (Read Path)](#attribution--pdp-authorization-read-path)
  - [UsageType Existence & Op-Kind Validation (Aggregated Path)](#usagetype-existence--op-kind-validation-aggregated-path)
  - [PDP Constraint Composition](#pdp-constraint-composition)
  - [Plugin SPI Aggregate Dispatch](#plugin-spi-aggregate-dispatch)
  - [Plugin SPI Raw Page Dispatch](#plugin-spi-raw-page-dispatch)
  - [Cursor Pagination Orchestration](#cursor-pagination-orchestration)
  - [Active & Inactive Record Visibility](#active--inactive-record-visibility)
- [4. States (CDSL)](#4-states-cdsl)
  - [Query Request Lifecycle State Machine](#query-request-lifecycle-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [FR: Aggregation Rule — SUM Nets, Others Usage Only](#fr-aggregation-rule--sum-nets-others-usage-only)
  - [FR: Query Aggregation](#fr-query-aggregation)
  - [FR: Query Raw](#fr-query-raw)
  - [FR: Tenant Isolation](#fr-tenant-isolation)
  - [NFR: Query Latency](#nfr-query-latency)
  - [NFR: Workload Isolation](#nfr-workload-isolation)
  - [NFR: Operational Visibility (Query-Path Instruments)](#nfr-operational-visibility-query-path-instruments)
  - [NFR: Authorization](#nfr-authorization)
  - [Principle: PDP-Centric Authorization](#principle-pdp-centric-authorization)
  - [Principle: Fail-Closed](#principle-fail-closed)
  - [Constraint: No Business Logic](#constraint-no-business-logic)
  - [Constraint: NFR Thresholds](#constraint-nfr-thresholds)
  - [Component: Query Gateway](#component-query-gateway)
  - [Sequence: Query Aggregated](#sequence-query-aggregated)
  - [Sequence: Query Raw](#sequence-query-raw)
  - [Contract: Downstream Usage Reader](#contract-downstream-usage-reader)
  - [Entity: AggregationQuery](#entity-aggregationquery)
  - [Entity: AggregationResult](#entity-aggregationresult)
  - [Entity: RawQuery](#entity-rawquery)
  - [Cursor: CursorV1 Toolkit Adoption](#cursor-cursorv1-toolkit-adoption)
  - [Entity: PdpConstraint](#entity-pdpconstraint)
  - [Entity: SecurityContext](#entity-securitycontext)
  - [Entity: ResourceRef](#entity-resourceref)
  - [API: POST /usage-collector/v1/records/aggregate](#api-post-usage-collectorv1recordsaggregate)
  - [API: GET /usage-collector/v1/records](#api-get-usage-collectorv1records)
  - [§2.4-item → DoD-ID Coverage Matrix](#24-item--dod-id-coverage-matrix)
- [6. Acceptance Criteria](#6-acceptance-criteria)
  - [6.1 Endpoints Summary](#61-endpoints-summary)
  - [6.2 Behavioural Criteria](#62-behavioural-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-featstatus-usage-query`

<!-- reference to DECOMPOSITION entry -->

- [ ] `p2` - `cpt-cf-usage-collector-feature-usage-query`

## 1. Feature Context

### 1.1 Overview

Provides the single, PDP-authorized read path into the metering substrate through one Query Gateway that serves two paths:

- **Aggregated** — `POST /usage-collector/v1/records/aggregate` with mandatory time range, mandatory single UsageType filter, and mandatory aggregation operator. Pushes server-side SUM / COUNT / MIN / MAX / AVG with grouping into the active storage plugin.
- **Raw** — `GET /usage-collector/v1/records?$filter=...&$orderby=...&limit=...&cursor=...` with mandatory time range expressed as `timestamp ge X and timestamp lt Y` inside `$filter`, optional OData narrowing predicates over the `UsageRecordFilterField` enum, toolkit `CursorV1` continuation decoded and validated at the gateway, and `limit` bounded by the page-size cap.

The `cpt-cf-usage-collector-component-query-gateway` accepts the caller's `SecurityContext` (resolved upstream by the ToolKit gateway on REST as `Extension<SecurityContext>` populated via `OperationBuilder::authenticated()`, or supplied verbatim by the in-process caller on the SDK trait). It authorizes every read through the per-component `access_scope_with` helper wrapping `cpt-cf-usage-collector-contract-authz-resolver` fail-closed.

User-supplied filters are composed with PDP-returned constraints so the authorized scope can only narrow. Both `active` and `inactive` `usage_records` within that scope are returned. The gateway fails closed on missing `SecurityContext`, PDP, or plugin unavailability. The write path lives in `cpt-cf-usage-collector-feature-usage-emission`.

**Consistency posture (read-after-write).** This feature's read surfaces (aggregated, raw, and the catalog reads they consult) inherit the gear-level consistency floor recorded in `cpt-cf-usage-collector-adr-consistency-contract` (ADR-0011) and DESIGN [§3.10](../DESIGN.md#310-consistency-contract): a record `Acknowledged` by the ingestion path MAY be invisible to a subsequent aggregated query, raw query, or catalog read for an indeterminate window.

**There is no read-your-writes guarantee against this feature**, and **no monotonic-reads-per-`(tenant_id, gts_id)` guarantee** — a record observed on one page or one aggregation MAY be missing from a later page or window against a different replica.

Caller flows that need same-request outcome (admission control, post-emit summary, immediate-readback dashboards) MUST consume the ingestion ack from `cpt-cf-usage-collector-feature-usage-emission`, not this feature. Near-real-time observers poll within `cpt-cf-usage-collector-nfr-query-latency` and accept lag bounded by the active plugin's published profile (`plugin-spi.md` §"Consistency profile"); consumers that need a tighter bound consciously couple to a specific plugin's ceiling.

### 1.2 Purpose

This feature exists so that downstream consumers (billing, dashboards, quota enforcers, tenant administrators) have a single, contract-stable read surface for usage data whose authorization posture is identical to the rest of the metering substrate — the per-component `access_scope_with` helper invocation against `cpt-cf-usage-collector-contract-authz-resolver` inside `cpt-cf-usage-collector-component-query-gateway` returns the PDP decision and constraint set fail-closed on the inbound `SecurityContext`, the mandatory single-UsageType reference on the aggregated path is validated deterministically per query via a `get_usage_type` SPI dispatch against `cpt-cf-usage-collector-contract-storage-plugin`, the usage-emission-owned `usage_records` table is consumed read-only (deactivation transitions remain owned by §2.5 Event Deactivation), and aggregation and raw record retrieval are delegated through the contract-stable Plugin SPI so the read shape is uniform regardless of the operator-selected storage backend. The Query Gateway refuses to widen scope under any user-supplied filter, rejects unregistered UsageType references with an actionable error envelope before plugin aggregate / raw dispatch (owned by §2.3 Usage Emission), returns an empty result set / page (not an error) on empty matches within the authorized scope, and preserves auditable history by returning both `active` and `inactive` rows within that scope.

**Requirements**: `cpt-cf-usage-collector-fr-query-aggregation`, `cpt-cf-usage-collector-fr-query-raw`, `cpt-cf-usage-collector-fr-tenant-isolation`, `cpt-cf-usage-collector-nfr-query-latency`, `cpt-cf-usage-collector-nfr-query-freshness`, `cpt-cf-usage-collector-nfr-workload-isolation`

**Principles**: `cpt-cf-usage-collector-principle-pdp-centric-authorization`, `cpt-cf-usage-collector-principle-fail-closed`

### 1.3 Actors

| Actor                                         | Role in Feature                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| --------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cpt-cf-usage-collector-actor-usage-consumer` | Any authenticated system that queries usage data through the public read surfaces (billing engines, quota enforcers, dashboards, downstream analytics) — submits aggregated reads via `POST /usage-collector/v1/records/aggregate` (aggregation-spec body plus `gts_id` / filter query parameters) or `SdkClient` aggregated-read operations, and raw reads via `GET /usage-collector/v1/records` with OData query parameters (`$filter`, `$orderby`, `$select`, `limit`, `cursor`) or `SdkClient` raw-read operations through the Query Gateway; subject to PDP authorization on every call, with the PDP-returned `PdpConstraint` set composed into the parsed `FilterNode<UsageRecordFilterField>` under intersection-only (narrowing) semantics; the SDK trait deliberately excludes UsageType catalog management per `sdk-trait.md` §Out of scope, so UsageType existence validation on the aggregated path dispatches `get_usage_type` directly against `cpt-cf-usage-collector-contract-storage-plugin` rather than through a separate SDK call |
| `cpt-cf-usage-collector-actor-tenant-admin`   | Tenant administrator who queries raw and aggregated usage data scoped to their own tenant via the same `POST /usage-collector/v1/records/aggregate` (body) and `GET /usage-collector/v1/records` (OData) paths (or the SDK equivalents); tenant isolation is enforced once by the per-component `access_scope_with` helper inside `cpt-cf-usage-collector-component-query-gateway` (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) and surfaced into the Query Gateway as a `PdpConstraint` set that narrows the authorized scope to the operator's tenant; cross-tenant reads are possible only when the platform PDP explicitly permits them (e.g., parent → subtenant hierarchies)                                                                                                                                                                                                                                                                                           |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) -- Aggregated Usage Query §5.5, Raw Usage Query §5.5, Tenant Isolation §5.3, Data Ownership and Stewardship §5.8, Data Lifecycle Delegation §5.8 (active-and-inactive visibility), UsageType Existence and Semantics Enforcement §5.7 (aggregated-path UsageType validation), Query Latency §6.1, Batch and Report Timing §6.1, Workload Isolation §6.1, Authorization Enforcement §6.1, Actor catalog §2 (Usage Consumer, Tenant Administrator)
- **Design**: [DESIGN.md](../DESIGN.md) -- Query Gateway component (§3) `cpt-cf-usage-collector-component-query-gateway`, Cursor & Pagination policy (§3.3) `cpt-cf-usage-collector-principle-cursor-gateway-ownership`, canonical-errors policy (§3.3) `cpt-cf-usage-collector-principle-canonical-errors`, Query Aggregated sequence (§3.6) `cpt-cf-usage-collector-seq-query-aggregated`, Query Raw sequence (§3.6) `cpt-cf-usage-collector-seq-query-raw`, `corrects_id` as the structural discriminator on every persisted usage record (`plugin-spi.md` §"Cross-entity invariants honored by the Plugin SPI"; read-only consumer; write surface declared by §2.3 Usage Emission), Correction posture two-primitive taxonomy and SUM-nets aggregation rule (`cpt-cf-usage-collector-adr-monotonic-deactivation` + `cpt-cf-usage-collector-adr-usage-compensation` — `SUM` is the signed net total; `COUNT`/`MIN`/`MAX`/`AVG` operate over active rows WHERE `corrects_id IS NULL`), Domain Model entities `AggregationQuery` / `AggregationResult` / `RawQuery` / `UsageRecordFilterField` / `Keyset` / `PdpConstraint` / `SecurityContext` / `ResourceRef` (§3.1) — raw paging is now expressed via `toolkit_odata::Page<UsageRecord>` plus the toolkit-internal `CursorV1`, PRD→DESIGN realization rows for `fr-query-aggregation`, `fr-query-raw`, `fr-tenant-isolation`, `fr-data-ownership`, `fr-usage-compensation` (read-side `SUM`-nets surface), `nfr-query-latency`, `nfr-batch-and-report-timing`, `nfr-workload-isolation`, `nfr-authorization`, `fr-data-lifecycle` (active-and-inactive visibility) (§5.3)
- **ADR**: [ADR/0008-usage-compensation.md](../ADR/0008-usage-compensation.md) -- `cpt-cf-usage-collector-adr-usage-compensation` — counter value-reversal primitive; the rationale for the `SUM`-nets / `COUNT`-`MIN`-`MAX`-`AVG`-over-`corrects_id IS NULL` aggregation contract surfaced by this feature; complemented by [ADR/0005-monotonic-deactivation.md](../ADR/0005-monotonic-deactivation.md) (`cpt-cf-usage-collector-adr-monotonic-deactivation`) for the uniform retraction primitive that applies regardless of `corrects_id` presence (deactivated rows are excluded from all five aggregations before netting); [ADR/0011-consistency-contract.md](../ADR/0011-consistency-contract.md) (`cpt-cf-usage-collector-adr-consistency-contract`) — floor-and-ceiling consistency contract that governs queryability lag on this feature's surfaces; the no-read-your-writes constraint surfaced in §1.1 above and the per-plugin ceiling discoverable through `plugin-spi.md` §"Consistency profile"
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) -- §2.4 Usage Query
- **Foundation feature**: [foundation.md](./foundation.md) -- SecurityContext acceptance at the surface boundaries (REST `Extension<SecurityContext>` from ToolKit gateway middleware via `OperationBuilder::authenticated()`; SDK trait methods accepting `ctx: &SecurityContext` as the first parameter), PDP enforcement via the per-component `access_scope_with` helper (`cpt-cf-usage-collector-flow-foundation-pdp-authorize`) returning the `(PdpDecision, PdpConstraint set)` envelope, plugin host binding, audit-correlation propagation, tenant isolation, fail-closed posture (reused, not re-defined)
- **UsageType Lifecycle feature**: [usage-type-lifecycle.md](./usage-type-lifecycle.md) -- platform-global usage-type catalog persisted in the plugin's `usage_type_catalog` table; the aggregated-path UsageType existence validation dispatches `get_usage_type` against `cpt-cf-usage-collector-contract-storage-plugin` per query- **Usage Emission feature**: [usage-emission.md](./usage-emission.md) -- write surface for usage records; the Query Gateway consumes the same records read-only via the Plugin SPI query capabilities and does not redefine ingestion semantics or the SPI dedup composite (reused, not re-defined)
- **Plugin SPI reference**: [plugin-spi.md](../plugin-spi.md) -- aggregated query capability (server-side SUM / COUNT / MIN / MAX / AVG with grouping push-down) and raw page retrieval capability invoked with a structured tuple `(filter_ast: FilterNode<UsageRecordFilterField>, order_keys: OrderKeys, page_after: Option<Keyset>, limit: u32)` returning `(rows: Vec<UsageRecord>, last_keyset: Option<Keyset>)`; the gateway dispatches both reads through these SPI capabilities and the plugin is opaque to the OData/cursor wire encoding
- **SDK trait reference**: [sdk-trait.md](../sdk-trait.md) -- aggregated and raw read operations routed through the Query Gateway (UsageType catalog management deliberately excluded per §Out of scope); `list_usage_records` returns `toolkit_odata::Page<UsageRecord>`
- **REST contract**: [usage-collector-v1.yaml](../usage-collector-v1.yaml) -- `POST /usage-collector/v1/records/aggregate` (typed body) and `GET /usage-collector/v1/records` (OData `$filter`, `$orderby`, `limit`, `cursor`) paths, the canonical `toolkit_canonical_errors::Problem` envelope, mandatory time-range (expressed as `timestamp ge X and timestamp lt Y` inside `$filter`) and (aggregated) mandatory single-UsageType filter validation, toolkit `CursorV1` continuation token, and `limit` bounded page size
- **Dependencies**: `cpt-cf-usage-collector-feature-foundation`, `cpt-cf-usage-collector-feature-usage-type-lifecycle`, `cpt-cf-usage-collector-feature-usage-emission`

### 1.5 Explicit Non-Applicability

- **UX** (`UX-FDESIGN-001` user journey, `UX-FDESIGN-002` accessibility): Not applicable because the usage-query feature is a backend read surface (`POST /usage-collector/v1/records/aggregate` and `GET /usage-collector/v1/records` plus the in-process SDK aggregated and raw read operations routed through the same Query Gateway); there is no human-facing UI in this gear, the only direct consumers are authenticated downstream systems (`cpt-cf-usage-collector-actor-usage-consumer`) and tenant administrators traversing the public read surfaces (`cpt-cf-usage-collector-actor-tenant-admin`), and any UI surfacing of usage data is delivered downstream by billing engines, dashboards, and analytics products outside this feature's scope. Developer experience on the read contract is encoded through the canonical `toolkit_canonical_errors::Problem` error envelopes, the toolkit `CursorV1` opaque continuation token (decoded and validated at the gateway via `toolkit_odata::validate_cursor_against`), and the `limit` bounded page size published by `usage-collector-v1.yaml` and `sdk-trait.md`.

### 1.6 Implementation Status

This subsection records the current state on both read paths
(`GET /usage-collector/v1/records` / `UsageCollectorClientV1::list_usage_records`
and `POST /usage-collector/v1/records/aggregate` /
`UsageCollectorClientV1::query_aggregated_usage_records`). Both surfaces
are wired end-to-end; the legacy aggregation-request body shape described
below in §§2-5 (with mandatory `time_range`, single-UsageType arity check,
typed `aggregation` operator on the wire) is **not** the shape actually
implemented. Treat those sections as forward-looking intent that has
since been superseded by the SDK-aligned shape described here.

The implemented body is `QueryAggregatedUsageRecordsRequest`, carrying
only `op` and `group_by`; `gts_id`, the OData `$filter`, and the
`metadata.<key>` filters are query parameters on the operation. The
schema names that once appeared in these sections — `AggregationRequest`,
`AggregationSpec`, `MetadataFilter` — no longer exist in
`usage-collector-v1.yaml`.

**Naming reconciliation.** Older spec passages reference a `(timestamp, id)` cursor keyset; the actual SDK field names are `created_at` and `id` and the implementation uses those throughout. Treat the spec's `timestamp` as a legacy synonym for `created_at` until the spec is fully refreshed.

**Implemented**:

- Raw-read SDK signature `UsageCollectorClientV1::list_usage_records(ctx, gts_id, query, metadata_filter)` is wired through `Service::list_usage_records` (see `gears/system/usage-collector/usage-collector/src/domain/service.rs`) and exposed at `GET /usage-collector/v1/records` (see `gears/system/usage-collector/usage-collector/src/api/rest/{routes,handlers}/usage_records.rs`).
- Aggregated-read SDK signature `UsageCollectorClientV1::query_aggregated_usage_records(ctx, gts_id, query, metadata_filter, aggregation)` is wired through `Service::query_aggregated_usage_records` (same file) and exposed at `POST /usage-collector/v1/records/aggregate`. The handler accepts `gts_id` and the `metadata.<key>` typed side-channel as query parameters (mirroring the raw path), the `[from, to)` time window flows through `$filter` as a `created_at` predicate (a bounded window is **mandatory** — the service requires both a lower (`created_at ge|gt …`) and an upper (`created_at le|lt …`) bound as top-level `$filter` conjuncts and rejects an absent or one-sided window with `400 InvalidArgument` / `MISSING_TIME_WINDOW`; the aggregate path has no `limit` ceiling, so the window is its only scan bound), and the JSON body carries only the `AggregationSpec` (operator + `group_by`). `$orderby`, `limit`, and `cursor` are NOT accepted on the aggregate path — aggregation is not paginated.
- The `TimeWindow` typed parameter has been **removed** from both the SDK trait and the plugin SPI; the `[from, to)` time window flows through `query.filter` as an ordinary `created_at ge … and created_at lt …` OData predicate. `UsageRecordQuery` now exposes `created_at` (`DateTimeUtc`) and `id` (`Uuid`) on its filterable schema for exactly that purpose. The gateway **requires** a bounded `created_at` window (a lower and an upper bound as top-level `$filter` conjuncts) on both the raw and aggregated read paths and rejects an absent or one-sided window with `400 InvalidArgument` / `MISSING_TIME_WINDOW` (`Service::{list,query_aggregated}_usage_records` → `domain::query::require_bounded_time_window`), so a caller cannot drive an unbounded full-table scan / aggregation. Only top-level conjuncts count — a `created_at` bound nested under `or` / `not` does not satisfy the requirement.
- `cpt-cf-usage-collector-flow-foundation-pdp-authorize` is invoked through the per-component `access_scope_with` helper (`PolicyEnforcer::access_scope_with`) against the `usage_record` resource with the `list` action verb. The PEP request is pre-row (no per-record attribution attributes) and uses `require_constraints(true)`: the returned `AccessScope` is projected to an OData filter (`domain::authz::scope_to_odata_filter`) and AND-merged with the caller's `$filter` under intersection-only semantics, and an unconstrained (`allow_all`) / empty-constraint permit fails closed (routed to the `denied` state, `context.reason="AUTHZ"`) rather than being treated as a happy path. The same `authorize_list_usage_records` helper authorizes both the raw and the aggregated read paths — the `list` action verb is shared because the read attribution tuple is identical at this stage (the aggregator's compute is downstream of authorization).
- `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2` is realized in `domain::service::compose_query_with_scope` + `domain::authz::scope_to_odata_filter`: every `AccessScope` constraint is projected to an `OData` filter expression over the `UsageRecordFilterField` surface (`OWNER_TENANT_ID` → `tenant_id`, `OWNER_ID` → `subject_id`, `PROP_RESOURCE_TYPE` → `resource_type`, `PROP_RESOURCE_ID` → `resource_id`, `PROP_SUBJECT_TYPE` → `subject_type`) and AND-merged into the caller's `$filter` under intersection-only semantics. The composer is shared by both `Service::list_usage_records` and `Service::query_aggregated_usage_records`. Tree predicates (`InGroup` / `InGroupSubtree` / `InTenantSubtree`), unknown PEP properties, and value-type mismatches surface as fail-closed `AuthorizationDenied` — the gear refuses to widen scope under a policy shape it cannot compile against a flat resource.
- `gts_id` is the only mandatory non-OData query parameter on the REST surface. `$filter` / `$orderby` / `limit` / `cursor` flow through the standard toolkit `OData` extractor.
- The `MetadataFilter` side-channel ships as repeated `metadata.<key>=<value>` query parameters: values for the same `<key>` collapse into a single `MetadataFilter` whose value set is OR-ed; distinct `<key>`s become independent filters AND-ed at the plugin. An empty key (`metadata.=…`) is a fail-closed canonical `InvalidArgument`. The handler also rejects any query parameter outside the declared set (`gts_id`, the OData parameters, and `metadata.<key>` entries) so a typo cannot silently widen the result set — mirroring `account-management::reject_non_odata_params`.
- Gateway-side guards on the parsed `ODataQuery` (handler `prepare_list_query`):
    - **`limit` bound** — per `cpt-cf-usage-collector-dod-usage-query-constraint-nfr-thresholds` and `prepare_list_query`, an **absent** `limit` defaults to `MAX_PAGE_SIZE = 1000`, while a present `limit > MAX_PAGE_SIZE` is **rejected** with `400 InvalidArgument` (`field_violations[0].field="limit"`, `.reason="VALIDATION"`) — it is NOT silently clamped, so a caller cannot misread a truncated page as complete.
    - **`$orderby` normalization (unique keyset tiebreaker)** — on every non-cursor request the gateway appends the canonical `(created_at, id)` suffix to the effective order via `toolkit_odata::ODataOrderBy::ensure_tiebreaker`, so the order always ends in a globally-unique key. When the caller omits `$orderby` this yields the canonical `(created_at asc, id asc)` keyset; when the caller supplies an `$orderby` that lacks a unique final key (e.g. `$orderby=created_at`, `$orderby=resource_id desc`), the missing `created_at` / `id` key(s) are appended so the effective order remains gap-free. The gateway — **not** the caller — owns the tiebreaker: a non-unique final sort key would let the plugin's keyset predicate silently drop rows sharing the boundary value that did not fit on the previous page (data loss across page boundaries). The tiebreaker is appended in the order's existing sort direction because this plugin's keyset supports only uniform-direction tuples; a descending `$orderby` therefore gets a descending suffix (never a mixed-direction tuple, which the plugin rejects). `ensure_tiebreaker` is a no-op for a key the order already names, so an order already ending in `id` is untouched. The plugin mints the next-page cursor's signed sort tokens from this normalized order, so cursor-continuation pages reconstruct the same suffix and page-to-page validation stays consistent. Realized in `prepare_list_query` (`gears/system/usage-collector/usage-collector/src/api/rest/handlers/usage_records.rs`). The plugin-owned catalog list (`GET /usage-collector/v1/usage-types`) is unaffected: it ignores `query.order` and paginates on the intrinsically-unique `gts_id` key.
    - **Cursor validation** — when `cursor` is present, the gateway calls `toolkit_odata::validate_cursor_against(&cursor, effective_order, filter_hash)` before plugin dispatch. All failures lift (via `toolkit_odata`'s `CanonicalError::from`) to `InvalidArgument` (HTTP 400) carrying a `field_violations[0]` on the `cursor` field: a malformed cursor → `reason="INVALID_CURSOR"`, `OrderMismatch` → `"ORDER_MISMATCH"`, `FilterMismatch` → `"FILTER_MISMATCH"` (and a malformed signed-token order → `"INVALID_ORDERBY_FIELD"` on `$orderby`). The validated `ODataQuery` (cursor included) is forwarded to the plugin unchanged; `@nextLink` minting is the plugin's responsibility via `Page::page_info.next_cursor`.
- The handler for `POST /usage-collector/v1/records/aggregate` reuses the raw path's `parse_required_gts_id` and `parse_metadata_filters` helpers, and rejects any query parameter outside `{gts_id, $filter, metadata.<key>}` via the aggregate-specific `reject_unknown_aggregate_params` (the aggregate path doesn't accept `$orderby`, `limit`, `cursor`, or `$select`); such pre-service parameter rejections surface as `400 InvalidArgument` with `field_violations[].reason="VALIDATION"` (the SDK `ValidationReason` catch-all) before the query pipeline runs. No metric is emitted here today — query telemetry is unwired (see Deferred).

**Deferred**:

- `cpt-cf-usage-collector-algo-usage-query-active-and-inactive-record-visibility` is honoured by construction (the gateway never filters on `status` or overrides it) but is not asserted as an explicit post-pass at the gateway.
- **Query operational telemetry is specified but NOT yet wired in gear source.** No meter instrument, no `QueryGuard`, and no telemetry emit exists yet for `uc_query_requests_total`, `uc_query_duration_seconds`, `uc_query_inflight`, or `uc_query_result_rows` (the successful-completion result-size observation — raw `items` length or aggregated `buckets` length — specified by `inst-raw-result-rows-observe` / `inst-aggregated-result-rows-observe`). The whole surface is owned by `cpt-cf-usage-collector-dod-usage-query-nfr-operational-visibility` and unchecked until the emit points land. When wired, `uc_query_inflight` is incremented *once authorization composes* per DESIGN §3.11.5 (not at bare service entry), and the completion counter/duration attach at the service boundary.
- **Pre-pipeline handler-boundary rejections are not counted.** Rejections that never enter the query pipeline — missing `Extension<SecurityContext>`, cursor decode/validate failures (`cursor_decode` / `order_mismatch` / `filter_mismatch` from `validate_cursor_against`), and the generic request-shape rejections carrying the `VALIDATION` `field_violations[].reason` (`limit` over `MAX_PAGE_SIZE`, unknown query parameters, malformed `gts_id` → `INVALID_BASE_GTS_ID`, unparseable `$filter` / `$orderby`, metadata-filter errors) — are refused at the REST handler before the service pipeline. DESIGN §3.11.5's closed `uc_query_requests_total` `error_category` set carries no generic `validation` category, so — mirroring the ingestion sibling's `inst-emit-batch-cap-check` treatment in `usage-emission.md` — these structural rejections are NOT recorded on the counter. (`cursor_decode` / `order_mismatch` / `filter_mismatch` and `missing_security_context` ARE in the closed set and would be recorded once handler-boundary telemetry is added; the SDK surface passes typed params and a required `ctx`, so it does not reach these rejections.) The one validation rejection that IS a recorded completion is the service-level missing / one-sided bounded-time-window guard (`require_bounded_time_window` → `MISSING_TIME_WINDOW`), recorded as `error_category="query_budget"` — the mandatory window is the query's scan-scope budget guard.
- The aggregated path resolves UsageType existence and the op-per-kind restriction pre-dispatch via `cpt-cf-usage-collector-algo-usage-query-usage-type-existence-on-aggregated-filter`: PDP authorize → enforce the mandatory bounded time window (`require_bounded_time_window`; missing → `InvalidArgument`, `field_violations[0].reason="MISSING_TIME_WINDOW"`) → resolve the usage type with a pre-dispatch `get_usage_type` (an unregistered `gts_id` → canonical `NotFound`, 404) → op-kind check (a mismatched `(op, kind)` → `InvalidArgument`, `.reason="OP_NOT_ALLOWED_FOR_KIND"`, 400) → compose scope → dispatch. What remains absent is the richer arity/semantics validator design: there is no arity check (the single `gts_id` is a typed required parameter, malformed → `INVALID_BASE_GTS_ID`), no `semantics_violation`, and no `unknown_usage_type` `context.reason` discriminator (the unregistered-type outcome is plain canonical `NotFound`, not a bespoke reason code). The `aggregation` operator is a closed enum (absent / unsupported → `InvalidArgument` at body deserialization).

## 2. Actor Flows (CDSL)

User-facing interactions that start with an actor (human or external system) and describe the end-to-end flow of a use case. Each flow has a triggering actor and shows how the system responds to actor actions.

### Query Aggregated

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-usage-query-query-aggregated`

**Actor**: `cpt-cf-usage-collector-actor-usage-consumer`

**Success Scenarios**:

- An authenticated usage consumer submits an aggregated read (via `POST /usage-collector/v1/records/aggregate` with an `AggregationRequest` body, or via the SDK `query_aggregated_usage_records` operation routed through `cpt-cf-usage-collector-component-query-gateway`) carrying a mandatory `time_range`, a mandatory single UsageType filter (`gts_id`), a mandatory `aggregation` operator (`SUM` / `COUNT` / `MIN` / `MAX` / `AVG` per the `AggregationOp` enum in `usage-collector-v1.yaml`), and optional `group_by` keys; `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read` resolves the caller into a `SecurityContext` and binds the `(PdpDecision, PdpConstraint set)` envelope to the request (the single `gts_id` is a typed required parameter — no arity check — and its existence is resolved by a pre-dispatch `get_usage_type` (Method 7) call, an unregistered `gts_id` surfacing as canonical `NotFound` (404) before dispatch; that resolution also yields the usage `kind`, which feeds a pre-dispatch op-kind check that rejects a mismatched `(op, kind)` pair — `SUM` on a gauge, or `MIN` / `MAX` / `AVG` on a counter — with `InvalidArgument` (`400`, `.reason="OP_NOT_ALLOWED_FOR_KIND"`; counter admits `{SUM, COUNT}`, gauge admits `{MIN, MAX, AVG, COUNT}`)), `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2` intersects the PDP constraint set with the user-supplied filters under intersection-only (narrowing) semantics, `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2` invokes the Plugin SPI `query_aggregated_usage_records` capability so the storage plugin executes the chosen aggregation and any `group_by` dimensions server-side, `cpt-cf-usage-collector-algo-usage-query-active-and-inactive-record-visibility` enforces that both `active` and `inactive` rows within the authorized scope contribute to the result, and the gateway returns a `AggregationResult` (`gts_id`, `aggregation`, `buckets`) per `usage-collector-v1.yaml`.
- A tenant administrator (`cpt-cf-usage-collector-actor-tenant-admin`) submits the same aggregated read scoped to their own tenant; the PDP-returned `PdpConstraint` set narrows the authorized scope to the operator's tenant via `cpt-cf-usage-collector-fr-tenant-isolation`, no cross-tenant rows are aggregated absent an explicit platform PDP permit, and the gateway returns the `AggregationResult` over that narrowed scope.
- An empty match within the authorized scope returns an `AggregationResult` with an empty `buckets` list per the Plugin SPI Method 3 contract — not an error envelope.

**Error Scenarios**:

- Request arrives without a resolved `SecurityContext` (REST handler did not receive `Extension<SecurityContext>` from ToolKit gateway middleware, or the SDK trait was invoked without a `ctx` argument) — whole-request rejection via the canonical `Unauthenticated` `Problem` envelope per `usage-collector-v1.yaml`; the collector never synthesizes identity and no plugin dispatch occurs.
- PDP denies the read attribution tuple — whole-request rejection via the propagated platform-authorization `Problem` envelope (`PermissionDenied`, `context.reason="AUTHZ"`) from `cpt-cf-usage-collector-flow-foundation-pdp-authorize`; no plugin dispatch occurs.
- The mandatory bounded `timestamp ge X and timestamp lt Y` window is missing from `$filter` — `InvalidArgument` (`field_violations[0].field="$filter"`, `.reason="MISSING_TIME_WINDOW"`) via the same `require_bounded_time_window` guard the raw path uses; no plugin dispatch occurs.
- The `gts_id` is malformed — `InvalidArgument` (`field_violations[0].field="gts_id"`, `.reason="INVALID_BASE_GTS_ID"`) at the `UsageTypeGtsId::new` boundary. There is no arity check (`gts_id` is a single typed required parameter). An unregistered `gts_id` is resolved pre-dispatch by a `get_usage_type` (Method 7) call and surfaces as canonical `NotFound` (404) — lifting the plugin's `UsageTypeNotFound` — before any aggregate dispatch.
- The `aggregation` operator is absent or not one of `{SUM, COUNT, MIN, MAX, AVG}` — rejected at request-body deserialization (the wire `op` is a closed enum) as `InvalidArgument` (HTTP `400`); no plugin dispatch occurs.
- The `aggregation` operator is not admitted by the resolved usage `kind` — `SUM` on a gauge, or `MIN` / `MAX` / `AVG` on a counter — `InvalidArgument` (HTTP `400`, `field_violations[0].field="aggregation.op"`, `.reason="OP_NOT_ALLOWED_FOR_KIND"`); the gateway resolves the `kind` via the pre-dispatch `get_usage_type` call and rejects the mismatch (counter admits `{SUM, COUNT}`; gauge admits `{MIN, MAX, AVG, COUNT}`) before any plugin aggregate dispatch.
- Plugin SPI `query_aggregated_usage_records` returns host-resolution `PluginUnavailable`, plugin-side `Transient`, or `Internal` — fail-closed `Problem` envelope per `usage-collector-v1.yaml`; the gateway never synthesizes a partial aggregation result and never caches a prior decision.

**Steps**:

1. [x] - `p1` - Caller submits an aggregated read — on REST through `POST /usage-collector/v1/records/aggregate` with an `AggregationRequest` body; the REST handler receives `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`) and W3C audit-correlation headers — or on the SDK through `UsageCollectorClientV1::query_aggregated_usage_records(ctx, ...)` with `ctx: &SecurityContext` as the first parameter per `sdk-trait.md` Method 3; the request carries mandatory `time_range`, mandatory single UsageType filter (`gts_id`), mandatory `aggregation` operator (`SUM` / `COUNT` / `MIN` / `MAX` / `AVG` per the `AggregationOp` enum in `usage-collector-v1.yaml`), optional `group_by` keys, and optional secondary filters (`tenant_id` / `resource_ref` / `subject_ref` / `status`) - `inst-aggregated-request-received`
2. [x] - `p1` - **IF** the REST handler receives no `Extension<SecurityContext>` (gateway middleware rejected the call upstream) or the SDK trait is invoked without a `ctx` argument **RETURN** the canonical `Unauthenticated` `Problem` envelope per `usage-collector-v1.yaml` default response; the collector never synthesizes identity - `inst-aggregated-missing-ctx`
3. [x] - `p1` - Delegate PDP authorization to `cpt-cf-usage-collector-flow-foundation-pdp-authorize` via the per-component `access_scope_with` helper inside `cpt-cf-usage-collector-component-query-gateway` (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) for the read attribution tuple, receiving the `(PdpDecision, PdpConstraint set)` envelope - `inst-aggregated-pdp-delegate`
4. [ ] - `p1` - **IF** the PDP decision is `deny` - `inst-aggregated-pdp-deny-branch`
   1. [x] - `p1` - **RETURN** the fail-closed platform-authorization `Problem` envelope (`PermissionDenied`, `context.reason="AUTHZ"`) per `usage-collector-v1.yaml` without any plugin dispatch (no cached decision) - `inst-aggregated-pdp-deny-return`
5. [x] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read` to bind the inbound `SecurityContext` and the `PdpConstraint` set to the validated request payload - `inst-aggregated-attribution`
   1. [ ] - `p1` - **IF** the algorithm returns a fail-closed `Problem` envelope (missing SecurityContext, missing PDP envelope, or empty PdpConstraint set per `inst-attribution-fail-closed-check`), **RETURN** that envelope verbatim without any further processing - `inst-aggregated-attribution-fail-return`
6. [x] - `p2` - Increment the `uc_query_inflight{query_kind="aggregated"}` gauge on query-gateway entry once authorization composes (the attribution binding above has bound the `SecurityContext` and the `PdpConstraint` set) per DESIGN [§3.11.5](../DESIGN.md#3115-operational-metric-inventory-ops-design-002); the gauge feeds the workload-isolation alert in DESIGN [§3.11.6](../DESIGN.md#3116-alerting-and-error-budget-architecture-ops-design-005) and is decremented by `inst-aggregated-telemetry-complete` on every exit path that follows this increment - `inst-aggregated-inflight-increment`
7. [ ] - `p1` - The `aggregation` operator is rejected at request-body deserialization when absent or outside `{SUM, COUNT, MIN, MAX, AVG}` (the wire `op` is a closed enum), surfacing as `InvalidArgument` (HTTP `400`) before this flow runs; the typed `gts_id` parameter is likewise validated at `UsageTypeGtsId::new` (malformed → `InvalidArgument`, `.reason="INVALID_BASE_GTS_ID"`) before dispatch - `inst-aggregated-structural-check`
8. [ ] - `p1` - **IF** the `$filter` lacks the mandatory bounded `timestamp ge X and timestamp lt Y` window (`require_bounded_time_window`) **RETURN** `InvalidArgument` (`field_violations[0].field="$filter"`, `.reason="MISSING_TIME_WINDOW"`) without any plugin dispatch - `inst-aggregated-time-window-check`
9. [ ] - `p1` - Resolve the queried usage type via `cpt-cf-usage-collector-algo-usage-query-usage-type-existence-on-aggregated-filter` with a pre-dispatch `get_usage_type` (Method 7) Plugin SPI call — an unregistered `gts_id` lifts the plugin's `UsageTypeNotFound` to canonical `NotFound` (404) here, before the aggregate dispatch — then invoke `require_op_allowed_for_kind(aggregation.op, usage_type.kind, gts_id)`: reject a mismatched `(op, kind)` pair (`SUM` on a gauge, or `MIN` / `MAX` / `AVG` on a counter) with `InvalidArgument` (HTTP `400`, `field_violations[0].field="aggregation.op"`, `.reason="OP_NOT_ALLOWED_FOR_KIND"`; counter admits `{SUM, COUNT}`, gauge admits `{MIN, MAX, AVG, COUNT}`) so the plugin never receives a mismatched pair (mirrors `inst-aggregated-existence-resolve` and `inst-aggregated-op-kind-check`) - `inst-aggregated-existence-and-op-kind-check`
10. [x] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2` to intersect the `PdpConstraint` set with the user-supplied filters (`group_by`, optional secondary filters) under intersection-only semantics; constraints can only narrow the authorized scope and MUST NOT widen it under any user-supplied input - `inst-aggregated-constraint-composition`
11. [x] - `p1` - **TRY** invoke `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2` to dispatch the composed filter set, the bounded time window, the typed `gts_id`, the chosen aggregation operator, and any `group_by` keys to the Plugin SPI `query_aggregated_usage_records` capability over the persisted usage records (records originate from `cpt-cf-usage-collector-component-ingestion-gateway` and are consumed read-only here; ingestion semantics are owned by §2.3 Usage Emission); the plugin executes SUM/COUNT/MIN/MAX/AVG and any `group_by` dimensions server-side per `plugin-spi.md` Method 3 - `inst-aggregated-plugin-dispatch`
12. [x] - `p1` - **CATCH** the Plugin SPI error from the aggregate dispatch: transport / readiness errors (`Transient`, no scoped client, `types-registry` unavailable) lift to `ServiceUnavailable`; other backend errors lift to `Internal`; **RETURN** the lifted `Problem` envelope (no synthesized partial result). An unregistered `gts_id` never reaches this dispatch — it is caught by the pre-dispatch `get_usage_type` resolution (step 9) and lifted to canonical `NotFound` (404) there - `inst-aggregated-plugin-catch`
    1. [x] - `p1` - **RETURN** the lifted `Problem` envelope (no synthesized partial result) per `usage-collector-v1.yaml` - `inst-aggregated-plugin-catch-return`
13. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-query-active-and-inactive-record-visibility` to confirm both `active` and `inactive` rows within the authorized scope contribute to the result (deactivation of `inactive` is owned by §2.5 Event Deactivation, not this feature) - `inst-aggregated-visibility-rule`
14. [ ] - `p1` - Assemble the `AggregationResult` (`gts_id`, `aggregation`, `buckets`) per `usage-collector-v1.yaml` - `inst-aggregated-result-assemble`
15. [x] - `p2` - Record the `uc_query_result_rows{query_kind="aggregated"}` histogram with the aggregated group count (the `buckets` length of the assembled `AggregationResult`, capped at 100,000 per `usage-collector-v1.yaml`) — recorded only when the query completes successfully, so that, read together with `uc_query_duration_seconds`, operators separate "slow because large" from "slow because degraded" per DESIGN [§3.11.5](../DESIGN.md#3115-operational-metric-inventory-ops-design-002) - `inst-aggregated-result-rows-observe`
16. [x] - `p2` - Record completion telemetry for the query attempt per DESIGN [§3.11.5](../DESIGN.md#3115-operational-metric-inventory-ops-design-002) (specified; **not yet wired** — no meter instrument exists today, see §1.6) through a completion guard established when the attempt is admitted at the query-gateway service boundary: on the guard's completion it observes the attempt's wall-clock seconds on `uc_query_duration_seconds{query_kind="aggregated"}` and increments `uc_query_requests_total{query_kind="aggregated", outcome, error_category}` on every terminal outcome at or after admission — including the `inst-aggregated-pdp-deny-return` (step 4.1) and `inst-aggregated-attribution-fail-return` (step 5.1) exits that return before the step-6 gauge increment — and it decrements `uc_query_inflight{query_kind="aggregated"}` only on exits that followed the `inst-aggregated-inflight-increment` bump (so the gauge never leaks under an early return and is never drained without a prior bump). The feature-owned exit→`(outcome, error_category)` mapping (DESIGN §3.11.5 supplies the closed value sets, not this projection) draws every category from the closed §3.11.5 `uc_query_requests_total` vocabulary: successful return → `(success, none)`; PDP `deny` (step 4.1) → `(denied, authz)`; the read path invokes PDP with `require_constraints(true)` per §1.6, so the attribution fail-closed (step 5.1) splits by branch — a permit whose returned `PdpConstraint` set is absent or empty is denied fail-closed → `(denied, authz)`, while a missing-`SecurityContext` / missing-`PdpDecision` substrate-unreachable exit → `(error, authz)`; missing / one-sided bounded time window (`inst-aggregated-time-window-check`, the service-level `require_bounded_time_window` guard) → `(error, query_budget)` — the mandatory window is the query's scan-scope budget guard; unregistered UsageType surfaced from the plugin as `NotFound` → `(error, unknown_usage_type)`; Plugin SPI transport / readiness / backend failure (`inst-aggregated-plugin-catch`) → `(error, plugin_error)`. The pre-pipeline rejections that run before the service guard is admitted are NOT recorded by this guard. The `inst-aggregated-missing-ctx` boundary check maps to the closed-set category `missing_security_context` and would be recorded only once REST-handler-boundary telemetry lands (deferred per §1.6; unreachable on the SDK surface, which passes a required `ctx`). The generic `inst-aggregated-structural-check` request-shape rejections (malformed `gts_id`, absent / unsupported `aggregation` operator, unknown query parameters) surface as `400 InvalidArgument` with `field_violations[].reason="VALIDATION"`, for which §3.11.5's closed set carries no category, so they are not recorded at all (mirroring the ingestion sibling's `inst-emit-batch-cap-check`, see §1.6 Deferred) - `inst-aggregated-telemetry-complete`
17. [x] - `p1` - **RETURN** the `AggregationResult` (with an empty `buckets` list when no rows match within the authorized scope — not an error) per `usage-collector-v1.yaml` - `inst-aggregated-return`

### Query Raw

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-flow-usage-query-query-raw`

**Actor**: `cpt-cf-usage-collector-actor-usage-consumer`

**Success Scenarios**:

- An authenticated usage consumer submits a raw read (via `GET /usage-collector/v1/records?$filter=...&$orderby=...&limit=...&cursor=...` with OData query parameters, or via the SDK `list_usage_records` operation routed through `cpt-cf-usage-collector-component-query-gateway`) where `$filter` is an OData predicate over `UsageRecordFilterField` carrying the mandatory `timestamp ge X and timestamp lt Y` time window plus optional narrowing predicates (`tenant_id` / `subject_id` / `subject_type` / `resource_id` / `resource_type` / `status`), `$orderby` is optional and normalized by the gateway to end in the canonical unique `(created_at, id)` suffix (an omitted `$orderby` becomes `created_at asc, id asc`), `limit` is bounded by the page-size cap, and `cursor` is an optional toolkit `CursorV1` continuation token; the gateway decodes and validates the cursor against the parsed `$filter` AST and `$orderby` projection via `toolkit_odata::validate_cursor_against` before any PDP or plugin work, enforces the semantic mandatoriness of the `timestamp ge X and timestamp lt Y` time-range window after OData parsing and before plugin dispatch, `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read` resolves the caller into a `SecurityContext` and binds the `(PdpDecision, PdpConstraint set)` envelope to the request, `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2` AND-merges the PDP constraint set into the parsed `FilterNode<UsageRecordFilterField>` under intersection-only (narrowing) semantics, `cpt-cf-usage-collector-algo-usage-query-cursor-pagination-orchestration-v2` projects the validated cursor to the plugin keyset `(created_at, id)`, `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2` invokes the Plugin SPI `list_usage_records` capability with the structured tuple `(filter_ast, order_keys, page_after, limit)` bounded by `limit`, `cpt-cf-usage-collector-algo-usage-query-active-and-inactive-record-visibility` enforces that both `active` and `inactive` rows within the authorized scope are returned, and the gateway mints the next `CursorV1` from `last_keyset` (when present) bound to the current `$filter` / `$orderby` and returns a `toolkit_odata::Page<UsageRecord>` envelope with `@nextLink` cursor URL per `usage-collector-v1.yaml`.
- Pagination continues across multiple calls by passing the prior response's `@nextLink` cursor token verbatim into the next request's `cursor` query parameter; the gateway decodes the cursor on every subsequent call, validates it against the current `$filter` / `$orderby`, and re-issues a fresh `CursorV1` from the next `last_keyset`; the plugin SPI is opaque to the cursor wire format and never decodes the token; the gateway omits `@nextLink` on the final page.
- A tenant administrator (`cpt-cf-usage-collector-actor-tenant-admin`) submits the same raw read scoped to their own tenant; the PDP-returned `PdpConstraint` set narrows the authorized scope to the operator's tenant via `cpt-cf-usage-collector-fr-tenant-isolation`, no cross-tenant rows are returned absent an explicit platform PDP permit, and the gateway returns the `toolkit_odata::Page<UsageRecord>` envelope over that narrowed scope.
- An empty match within the authorized scope returns a `toolkit_odata::Page<UsageRecord>` with an empty `items` list (and no `@nextLink`) per the Plugin SPI Method 4 contract — not an error envelope.

**Error Scenarios**:

- Request arrives without a resolved `SecurityContext` (REST handler did not receive `Extension<SecurityContext>` from ToolKit gateway middleware, or the SDK trait was invoked without a `ctx` argument) — whole-request rejection via the canonical `Unauthenticated` `toolkit_canonical_errors::Problem` envelope per `usage-collector-v1.yaml`; the collector never synthesizes identity and no plugin dispatch occurs.
- PDP denies the read attribution tuple — whole-request rejection via the propagated platform-authorization `Problem` envelope (`PermissionDenied`, `context.reason="AUTHZ"`) from `cpt-cf-usage-collector-flow-foundation-pdp-authorize`; no plugin dispatch occurs.
- The supplied `cursor` fails `CursorV1` decode (malformed payload or version tag) — `InvalidArgument` (`field_violations[0].field="cursor"`, `.reason="INVALID_CURSOR"`); no plugin dispatch occurs.
- The supplied `cursor` was minted against a different `$orderby` projection — `InvalidArgument` (`field_violations[0].field="cursor"`, `.reason="ORDER_MISMATCH"`); a `$orderby` that mixes `asc` and `desc` across keys, or names an optional (nullable) field, surfaces `.reason="VALIDATION"` on `$orderby` — both are keyset-soundness rejections raised in `prepare_list_query`, not `$orderby` parse failures (an unparseable `$orderby` is the toolkit's `.reason="INVALID_ORDERBY_FIELD"`); no plugin dispatch occurs.
- The supplied `cursor` was minted against a different `$filter` AST — `InvalidArgument` (`field_violations[0].field="cursor"`, `.reason="FILTER_MISMATCH"`). A missing mandatory `timestamp ge X and timestamp lt Y` window surfaces separately as `.reason="MISSING_TIME_WINDOW"` on `$filter`. No plugin dispatch occurs.
- `limit` exceeds the bounded cap of 1,000 records per page declared in `usage-collector-v1.yaml` — rejected gateway-side (`prepare_list_query`) with the canonical `InvalidArgument` `Problem` (`field_violations[0].field="limit"`, `.reason="VALIDATION"`), never clamped; an absent `limit` defaults to the cap. No plugin dispatch happens with an out-of-cap limit.
- Plugin SPI `list_usage_records` returns host-resolution `PluginUnavailable`, plugin-side `Transient`, or `Internal` — fail-closed `Problem` envelope per `usage-collector-v1.yaml`; the gateway never synthesizes a partial page and never caches a prior decision.

**Steps**:

1. [x] - `p1` - Caller submits a raw read — on REST through `GET /usage-collector/v1/records?$filter=...&$orderby=...&limit=...&cursor=...` with OData query parameters; the REST handler receives `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`) and W3C audit-correlation headers — or on the SDK through `UsageCollectorClientV1::list_usage_records(ctx, ...)` with `ctx: &SecurityContext` as the first parameter per `sdk-trait.md` Method 4; the request carries the `$filter` predicate over `UsageRecordFilterField` (mandatory `timestamp ge X and timestamp lt Y` window plus optional narrowing predicates over `tenant_id` / `subject_id` / `subject_type` / `resource_id` / `resource_type` / `status` per `usage-collector-v1.yaml`), an optional `$orderby` the gateway normalizes to end in the canonical unique `(created_at, id)` suffix, `limit` bounded by the page-size cap, and an optional `cursor` (toolkit `CursorV1`) - `inst-raw-request-received`
2. [x] - `p1` - **IF** the REST handler receives no `Extension<SecurityContext>` (gateway middleware rejected the call upstream) or the SDK trait is invoked without a `ctx` argument **RETURN** the canonical `Unauthenticated` `Problem` envelope per `usage-collector-v1.yaml` default response; the collector never synthesizes identity - `inst-raw-missing-ctx`
3. [x] - `p1` - Delegate PDP authorization to `cpt-cf-usage-collector-flow-foundation-pdp-authorize` via the per-component `access_scope_with` helper inside `cpt-cf-usage-collector-component-query-gateway` (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) for the read attribution tuple, receiving the `(PdpDecision, PdpConstraint set)` envelope - `inst-raw-pdp-delegate`
4. [ ] - `p1` - **IF** the PDP decision is `deny` - `inst-raw-pdp-deny-branch`
   1. [x] - `p1` - **RETURN** the fail-closed platform-authorization `Problem` envelope (`context.reason="AUTHZ"`) per `usage-collector-v1.yaml` without any plugin dispatch (no cached decision) - `inst-raw-pdp-deny-return`
5. [x] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read` to bind the inbound `SecurityContext` and the `PdpConstraint` set to the validated request payload - `inst-raw-attribution`
   1. [ ] - `p1` - **IF** the algorithm returns a fail-closed `Problem` envelope (missing SecurityContext, missing PDP envelope, or empty PdpConstraint set per `inst-attribution-fail-closed-check`), **RETURN** that envelope verbatim without any further processing - `inst-raw-attribution-fail-return`
6. [x] - `p2` - Increment the `uc_query_inflight{query_kind="raw"}` gauge on query-gateway entry once authorization composes (the attribution binding above has bound the `SecurityContext` and the `PdpConstraint` set) per DESIGN [§3.11.5](../DESIGN.md#3115-operational-metric-inventory-ops-design-002); the gauge feeds the workload-isolation alert in DESIGN [§3.11.6](../DESIGN.md#3116-alerting-and-error-budget-architecture-ops-design-005) and is decremented by `inst-raw-telemetry-complete` on every exit path that follows this increment - `inst-raw-inflight-increment`
7. [x] - `p1` - Parse `$filter`, `$orderby`, and `limit` via toolkit-odata; in `prepare_list_query` an absent `limit` defaults to `MAX_PAGE_SIZE = 1000` while a present `limit > MAX_PAGE_SIZE` is **rejected** gateway-side with the canonical `InvalidArgument` `Problem` (HTTP `400`, `field_violations[0].field="limit"`, `.reason="VALIDATION"`) — never silently clamped; on unparseable OData expressions return the canonical `Problem` envelope (HTTP `400`) per `usage-collector-v1.yaml` without any plugin dispatch - `inst-raw-odata-parse`
    1. [x] - `p1` - Parse the `metadata.<key>` query parameters into the typed `MetadataFilter` set (an empty key is rejected as a canonical `InvalidArgument` `Problem`; an empty value is admitted verbatim) - `inst-raw-metadata-filter-parse`
8. [ ] - `p1` - **IF** the parsed `$filter` AST does NOT contain the mandatory `timestamp ge X and timestamp lt Y` time-range window (semantic mandatoriness enforced by the usage-query algorithm at the gateway after OData parsing and before plugin dispatch — NOT by toolkit-odata) - `inst-raw-time-range-mandatory-check`
   1. [ ] - `p1` - **RETURN** the canonical `InvalidArgument` `Problem` (`field_violations[0].field="$filter"`, `.reason="MISSING_TIME_WINDOW"`) per `usage-collector-v1.yaml` without any plugin dispatch - `inst-raw-time-range-mandatory-return`
9. [ ] - `p1` - **IF** the request carries a `cursor` query parameter - `inst-raw-cursor-validate-branch`
   1. [x] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-query-cursor-pagination-orchestration-v2` to decode the `cursor` as a toolkit `CursorV1` value and validate it via `toolkit_odata::validate_cursor_against` against the parsed `$filter` AST and `$orderby` projection; every failure lifts to `InvalidArgument` with a `field_violations[0]` on `cursor` — malformed → `.reason="INVALID_CURSOR"`, `OrderMismatch` → `"ORDER_MISMATCH"`, `FilterMismatch` → `"FILTER_MISMATCH"`; no plugin dispatch in any of these branches - `inst-raw-cursor-validate`
10. [x] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2` to AND-merge the `PdpConstraint` set into the parsed `FilterNode<UsageRecordFilterField>` AST under intersection-only semantics; PDP constraints can only narrow the authorized scope and MUST NOT widen it under any user-supplied input; the resulting AST is the single source of truth handed to the plugin SPI (no separate constraint envelope is forwarded) - `inst-raw-constraint-composition`
11. [x] - `p1` - **TRY** invoke `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2` to dispatch the structured tuple `(filter_ast: FilterNode<UsageRecordFilterField>, order_keys: OrderKeys, page_after: Option<Keyset>, limit: u32)` to the Plugin SPI `list_usage_records` capability over the persisted usage records (records originate from `cpt-cf-usage-collector-component-ingestion-gateway` and are consumed read-only here; ingestion semantics are owned by §2.3 Usage Emission) — the cursor wire format is NEVER forwarded to the plugin; the plugin returns `(rows: Vec<UsageRecord>, last_keyset: Option<Keyset>)` - `inst-raw-plugin-dispatch`
12. [x] - `p1` - **CATCH** Plugin SPI transport, readiness, or contract error (host-resolution `PluginUnavailable`, plugin-side `Transient`, or `Internal`) - `inst-raw-plugin-catch`
    1. [ ] - `p1` - **RETURN** the fail-closed `Problem` envelope per `usage-collector-v1.yaml` (no synthesized partial page) - `inst-raw-plugin-catch-return`
13. [ ] - `p1` - Invoke `cpt-cf-usage-collector-algo-usage-query-active-and-inactive-record-visibility` to confirm both `active` and `inactive` rows within the authorized scope are included in the returned page (deactivation of `inactive` is owned by §2.5 Event Deactivation, not this feature) - `inst-raw-visibility-rule`
14. [ ] - `p1` - Mint the next `CursorV1` from the plugin-returned `last_keyset` (when present) bound to the current parsed `$filter` AST and `$orderby` projection; assemble the `toolkit_odata::Page<UsageRecord>` envelope (`items`, optional `@nextLink` containing the minted cursor token) per `usage-collector-v1.yaml`; omit `@nextLink` when the plugin signaled the last page (`last_keyset` absent) - `inst-raw-page-assemble`
15. [x] - `p2` - Record the `uc_query_result_rows{query_kind="raw"}` histogram with the raw page size (the `items` length of the assembled `toolkit_odata::Page<UsageRecord>`, ≤ 1,000 by the `limit` bound) — recorded only when the query completes successfully, so that, read together with `uc_query_duration_seconds`, operators separate "slow because large" from "slow because degraded" per DESIGN [§3.11.5](../DESIGN.md#3115-operational-metric-inventory-ops-design-002) - `inst-raw-result-rows-observe`
16. [x] - `p2` - Record completion telemetry for the query attempt per DESIGN [§3.11.5](../DESIGN.md#3115-operational-metric-inventory-ops-design-002) (specified; **not yet wired** — no meter instrument exists today, see §1.6) through a completion guard established when the attempt is admitted at the query-gateway service boundary: on the guard's completion it observes the attempt's wall-clock seconds on `uc_query_duration_seconds{query_kind="raw"}` and increments `uc_query_requests_total{query_kind="raw", outcome, error_category}` on every terminal outcome at or after admission — including the `inst-raw-pdp-deny-return` (step 4.1) and `inst-raw-attribution-fail-return` (step 5.1) exits that return before the step-6 gauge increment — and it decrements `uc_query_inflight{query_kind="raw"}` only on exits that followed the `inst-raw-inflight-increment` bump (so the gauge never leaks under an early return and is never drained without a prior bump). The feature-owned exit→`(outcome, error_category)` mapping (DESIGN §3.11.5 supplies the closed value sets, not this projection) draws every category from the closed §3.11.5 `uc_query_requests_total` vocabulary: successful return → `(success, none)`; PDP `deny` (step 4.1) → `(denied, authz)`; the read path invokes PDP with `require_constraints(true)` per §1.6, so the attribution fail-closed (step 5.1) splits by branch — a permit whose returned `PdpConstraint` set is absent or empty is denied fail-closed → `(denied, authz)`, while a missing-`SecurityContext` / missing-`PdpDecision` substrate-unreachable exit → `(error, authz)`; missing / one-sided bounded time window (`inst-raw-time-range-mandatory-check`, the service-level `require_bounded_time_window` guard) → `(error, query_budget)` — the mandatory window is the query's scan-scope budget guard; unregistered UsageType surfaced from the plugin as `NotFound` → `(error, unknown_usage_type)`; Plugin SPI transport / readiness / backend failure (`inst-raw-plugin-catch`) → `(error, plugin_error)`. The pre-pipeline handler rejections that run before the service guard is admitted are NOT recorded by this guard: the cursor decode / validation failures (`inst-raw-cursor-validate`; `INVALID_CURSOR` / `ORDER_MISMATCH` / `FILTER_MISMATCH`) and the missing-`SecurityContext` boundary check map to the closed-set categories `cursor_decode` / `order_mismatch` / `filter_mismatch` / `missing_security_context` and would be captured only once REST-handler-boundary telemetry lands (deferred per §1.6; unreachable on the SDK surface, which passes a required `ctx` and typed params); the generic request-shape rejections — unparseable `$filter` / `$orderby` (`inst-raw-odata-parse`), `limit` above the bounded cap, unknown query parameters, malformed `gts_id` — surface as `400 InvalidArgument` with `field_violations[].reason="VALIDATION"` for which §3.11.5's closed set carries no category, so they are not recorded at all (mirroring the ingestion sibling's `inst-emit-batch-cap-check`) - `inst-raw-telemetry-complete`
17. [x] - `p1` - **RETURN** the `toolkit_odata::Page<UsageRecord>` envelope (with an empty `items` list and no `@nextLink` when no rows match within the authorized scope — not an error) per `usage-collector-v1.yaml` - `inst-raw-return`

## 3. Processes / Business Logic (CDSL)

Internal system functions and procedures that do not interact with actors directly. These are reusable building blocks called by Actor Flows or other processes.

### Attribution & PDP Authorization (Read Path)

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read`

**Input**: The inbound `SecurityContext` received at the `cpt-cf-usage-collector-component-query-gateway` boundary (on REST as `Extension<SecurityContext>` from ToolKit gateway middleware, on SDK as the `ctx: &SecurityContext` first argument), the `(PdpDecision, PdpConstraint set)` envelope from `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (invoked via the per-component `access_scope_with` helper inside the query gateway), and the read request payload (aggregated or raw).

**Output**: An attributed read request that binds the resolved `SecurityContext` and the `PdpConstraint` set to the validated request payload, ready for downstream filter composition. The algorithm discriminates fail-closed outcomes by branch so the calling flow surfaces the correct §4 state and `Problem` envelope category: (a) missing resolved `SecurityContext` OR missing `PdpDecision` envelope (substrate unreachable / no decision available) yields a fail-closed `Problem` envelope routed to the `unavailable` state per `usage-collector-v1.yaml`; (b) substrate returned `permit` without an accompanying `PdpConstraint` set OR an empty constraint set (no permitted rows in any dimension) yields a fail-closed `Problem` envelope (`context.reason="AUTHZ"`) routed to the `denied` state per `usage-collector-v1.yaml`. In every fail-closed branch: no synthesized identity, no cached decision, no inferred result.

**Steps**:

1. [ ] - `p1` - Receive the inbound `SecurityContext` at the `cpt-cf-usage-collector-component-query-gateway` boundary — on REST as `Extension<SecurityContext>` from ToolKit gateway middleware, on SDK as the `ctx: &SecurityContext` first argument (the calling flow already exited fail-closed if `Extension<SecurityContext>` was absent on REST or `ctx` was absent on SDK) - `inst-attribution-receive-secctx`
2. [ ] - `p1` - Receive the `(PdpDecision, PdpConstraint set)` envelope from `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (invoked via the per-component `access_scope_with` helper inside the query gateway; the calling flow already exited fail-closed on PDP `deny`) - `inst-attribution-receive-pdp`
3. [ ] - `p1` - **IF** the resolved `SecurityContext` is missing, OR the `PdpDecision` envelope is missing, OR the substrate returned `permit` without an accompanying `PdpConstraint` set, OR the accompanying `PdpConstraint` set is empty (no permitted rows in any dimension) - `inst-attribution-fail-closed-check`
   1. [ ] - `p1` - **IF** the resolved `SecurityContext` is missing OR the `PdpDecision` envelope is missing (substrate unreachable / no decision available) — **RETURN** the fail-closed `Problem` envelope routed to the `unavailable` state per `usage-collector-v1.yaml` (no synthesized identity, no cached decision) - `inst-attribution-fail-closed-substrate-return`
   2. [ ] - `p1` - **ELSE** (substrate returned `permit` without an accompanying `PdpConstraint` set OR the constraint set is empty) — **RETURN** the fail-closed `Problem` envelope (`context.reason="AUTHZ"`) routed to the `denied` state per `usage-collector-v1.yaml` (PDP authorized no rows in the requested scope; no inferred result) - `inst-attribution-fail-closed-return`
4. [ ] - `p1` - Bind the resolved `SecurityContext` and the `PdpConstraint` set to the validated request payload as an attributed read request, preserving the foundation-resolved tenant scope - `inst-attribution-bind`
5. [ ] - `p1` - **RETURN** the attributed read request to the caller - `inst-attribution-return`

### UsageType Existence & Op-Kind Validation (Aggregated Path)

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-algo-usage-query-usage-type-existence-on-aggregated-filter`

**Input**: The typed `gts_id: UsageTypeGtsId` (parsed at the `UsageTypeGtsId::new` boundary — a malformed value is rejected as `InvalidArgument`, `.reason="INVALID_BASE_GTS_ID"`) and the requested `aggregation.op`.

**Output**: The resolved `UsageType` (carrying `kind`) on success, or a fail-closed rejection: an unregistered `gts_id` surfaces as canonical `NotFound` (`404`), and an `(op, kind)` pair the kind does not admit surfaces as `InvalidArgument` (`400`, `.reason="OP_NOT_ALLOWED_FOR_KIND"`). Both are produced BEFORE any Plugin SPI aggregate dispatch.

**Steps**:

1. [ ] - `p1` - After PDP authorization and the bounded-time-window check, resolve the usage type with a per-query `get_usage_type` Plugin SPI dispatch — an unregistered `gts_id` lifts the plugin's `UsageTypeNotFound` to canonical `NotFound` (`404`) here, before the aggregate dispatch - `inst-aggregated-existence-resolve`
2. [ ] - `p1` - Reject the request with `InvalidArgument` (`400`, `.reason="OP_NOT_ALLOWED_FOR_KIND"`) when `aggregation.op` is not admitted by the resolved `UsageType.kind` — counter admits `{SUM, COUNT}`, gauge admits `{MIN, MAX, AVG, COUNT}` (the matrix is owned by `AggregationOp::is_allowed_for`); the plugin stays pure-persistence and never receives a mismatched pair - `inst-aggregated-op-kind-check`

### PDP Constraint Composition

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2`

**Input**: The `PdpConstraint` set from `cpt-cf-usage-collector-flow-foundation-pdp-authorize`, the parsed `FilterNode<UsageRecordFilterField>` AST from the raw read (or the typed user-supplied filters from the legacy aggregated request body — `gts_id`, `tenant_id`, `resource_ref`, `subject_ref`, `status`, plus optional `group_by`), and the resolved `SecurityContext` for tenant anchoring.

**Output**: A composed filter expression whose authorized scope is the intersection of the `PdpConstraint` set and the user-supplied filters. Composition is intersection-only: the gateway AND-merges PDP constraint predicates into the client filter AST (or the aggregated typed filter map). The resulting AST is the single source of truth handed to the plugin SPI — no separate constraint envelope is forwarded. Any user-supplied attempt to widen scope beyond a PDP constraint is clamped back to the constraint bound. No widening, no scope expansion under any user-supplied input.

**Steps**:

1. [ ] - `p1` - Receive the parsed `FilterNode<UsageRecordFilterField>` AST from the raw-read OData parser (or the typed user-supplied filter map from the legacy aggregated request body) - `inst-constraint-composition-parse-user`
2. [ ] - `p1` - Parse the `PdpConstraint` set from the `(PdpDecision, PdpConstraint set)` envelope returned by `cpt-cf-usage-collector-flow-foundation-pdp-authorize` - `inst-constraint-composition-parse-pdp`
3. [ ] - `p1` - **FOR EACH** constraint in the `PdpConstraint` set - `inst-constraint-composition-iterate`
   1. [ ] - `p1` - AND-merge the constraint predicate with the matching dimension in the parsed `FilterNode<UsageRecordFilterField>` AST (or the aggregated typed filter map for `tenant_id`, `resource_ref`, `subject_ref`, `gts_id`, `status`); when no matching user-supplied predicate exists for that dimension, append the constraint predicate as-is so the authorized scope is narrowed by the constraint alone; when a user-supplied predicate exists on a dimension that has NO matching PDP constraint, the user-supplied predicate is preserved as-is (PDP imposed no bound on that dimension) - `inst-constraint-composition-intersect`
4. [ ] - `p1` - **IF** any user-supplied predicate attempts to widen scope beyond a `PdpConstraint` bound (e.g. requesting a tenant outside the PDP-permitted tenants, or a UsageType outside a PDP-permitted UsageType set) - `inst-constraint-composition-widen-check`
   1. [ ] - `p1` - Narrow the user-supplied predicate back to the `PdpConstraint` bound (no widening permitted under any user-supplied input; the clamp is silent on the wire and observable only in the narrowed result scope, never as a `Problem` envelope) - `inst-constraint-composition-clamp`
5. [ ] - `p1` - **RETURN** the composed `FilterNode<UsageRecordFilterField>` AST (or the composed aggregated typed filter map) anchored on the resolved `SecurityContext` for downstream Plugin SPI dispatch (`cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2` or `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2`) — the AST is the single source of truth; no separate constraint envelope is forwarded - `inst-constraint-composition-return`

### Plugin SPI Aggregate Dispatch

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2`

**Input**: The composed filter set from `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2`, the mandatory bounded time window, the typed `gts_id: UsageTypeGtsId` (parsed at the `UsageTypeGtsId::new` boundary; existence AND `kind` resolved by a pre-dispatch `get_usage_type` (an unregistered `gts_id` surfaces as `NotFound` before this dispatch; an `(op, kind)` pair the kind does not admit surfaces as `InvalidArgument`, `.reason="OP_NOT_ALLOWED_FOR_KIND"`, before this dispatch)), the chosen aggregation operator (`SUM` / `COUNT` / `MIN` / `MAX` / `AVG`), and any optional `group_by` keys.

**Output**: A `AggregationResult` (`gts_id`, `aggregation`, `buckets`) returned by the Plugin SPI `query_aggregated_usage_records` capability per `plugin-spi.md` Method 3 — the plugin executes the chosen aggregation and any `group_by` dimensions server-side using its native acceleration structures, bounded by the wire-level caps declared in `usage-collector-v1.yaml` (≤ 100,000 rows over a 90-day single-tenant window with ≤ 2 groupings). On host-resolution `PluginUnavailable` / plugin-side `Transient` / `Internal`, a fail-closed `Problem` envelope per `usage-collector-v1.yaml`. The `-v2` suffix denotes the canonical aggregation contract anchored on `corrects_id` presence.

**Aggregation rule (locked; encoded in the dispatch request and honoured by the plugin per `plugin-spi.md` Method 3)** — `SUM(value)` is computed across active rows regardless of `corrects_id` presence, treating `value` as a signed quantity, so `SUM` is the **signed net total** per `(tenant_id, gts_id)` group: rows with `corrects_id IS NOT NULL` (counter compensations) carry a strictly-negative `value` and reduce the running counter total. `COUNT`, `MIN`, `MAX`, and `AVG` operate over active rows WHERE `corrects_id IS NULL` — rows with `corrects_id IS NOT NULL` are excluded from these four aggregations before they are computed. **Compensation entries adjust SUM; they are not events.** Counting a compensation as an event would double-count the original usage event (the row referenced by `corrects_id` is already counted); including a compensation's strictly-negative `value` in `MIN` / `MAX` / `AVG` would corrupt extremes (a refund would always become the new `MIN`) and corrupt means (the arithmetic mean would drift below the observed usage range). Status filtering applies before aggregation — deactivated rows are excluded from every aggregation regardless of `corrects_id` presence; the `active`-status filter and the `corrects_id`-presence filter are orthogonal. A negative `SUM(value)` is an ordinary aggregation outcome — the Usage Collector does NOT validate non-negative net and does NOT emit a negative-net detection signal per the un-policed-net stance in `cpt-cf-usage-collector-adr-usage-compensation`; downstream consumers own any "net can't be negative" policy.

**Steps**:

1. [ ] - `p1` - Assemble the Plugin SPI `query_aggregated_usage_records` request (composed filter set, mandatory `time_range`, validated UsageType handle, aggregation operator, optional `group_by` keys) per the contract published in `plugin-spi.md` Method 3; encode the aggregation rule (`SUM` nets across active rows regardless of `corrects_id` presence; `COUNT` / `MIN` / `MAX` / `AVG` filter to active rows WHERE `corrects_id IS NULL` before aggregating) so the plugin executes the `corrects_id`-aware operator server-side — the rule is part of the operator contract, not a post-filter applied at the gateway - `inst-aggregate-dispatch-assemble-v2`
2. [ ] - `p1` - **TRY** invoke the storage-plugin `query_aggregated_usage_records` capability via `cpt-cf-usage-collector-component-plugin-host` over the persisted usage records — the plugin treats every filter as authoritative and MUST NOT widen the result set beyond the supplied filters, MUST honour the `corrects_id`-aware aggregation rule (`SUM` nets signed values across active rows regardless of `corrects_id` presence; `COUNT` / `MIN` / `MAX` / `AVG` over active rows WHERE `corrects_id IS NULL`), and executes the chosen operator plus any `group_by` dimensions server-side (fanning out per-row reads to the core is forbidden per `plugin-spi.md` Method 3) - `inst-aggregate-dispatch-try-v2`
3. [ ] - `p1` - **CATCH** Plugin SPI error (host-resolution `PluginUnavailable`, plugin-side `Transient`, or `Internal` per `plugin-spi.md` Method 3) - `inst-aggregate-dispatch-catch-v2`
   1. [ ] - `p1` - **RETURN** the fail-closed `Problem` envelope per `usage-collector-v1.yaml` (no synthesized partial result) - `inst-aggregate-dispatch-catch-return-v2`
4. [ ] - `p1` - **RETURN** the `AggregationResult` (with an empty `buckets` list when no rows match within the authorized scope — not an error per `plugin-spi.md` Method 3); a negative `SUM(value)` bucket is an ordinary aggregation outcome and MUST NOT be rewritten or rejected by the gateway per the un-policed-net stance in `cpt-cf-usage-collector-adr-usage-compensation` - `inst-aggregate-dispatch-return-v2`

### Plugin SPI Raw Page Dispatch

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2`

**Input**: The validated `gts_id: UsageTypeGtsId` (parsed at the `UsageTypeGtsId::new` boundary and threaded as the typed named parameter on the Plugin SPI signature), the composed `FilterNode<UsageRecordFilterField>` AST from `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2` (carrying the mandatory bounded `created_at` `[from, to)` window as `created_at ge … and created_at lt …` conjuncts, plus optional narrowing predicates over `tenant_id` / `subject_id` / `subject_type` / `resource_id` / `resource_type` / `status` — `gts_id` is deliberately absent from `UsageRecordFilterField`, so a `gts_id`-touching `$filter` is rejected at parse time as `FilterError::UnknownField` and can never reach this AST), the normalized `OrderKeys` projection (the caller's `$orderby`, if any, ending in the canonical unique `(created_at, id)` suffix), the optional `page_after: Option<Keyset>` projected from the validated `CursorV1` by `cpt-cf-usage-collector-algo-usage-query-cursor-pagination-orchestration-v2`, and the `limit: u32` resolved from the `limit` query parameter, ≤ 1,000 records per page (absent → 1,000; a present value above the cap is rejected before dispatch, not clamped).

**Output**: A `(rows: Vec<UsageRecord>, last_keyset: Option<Keyset>)` tuple returned by the Plugin SPI `list_usage_records` capability per `plugin-spi.md` Method 4 — the plugin emits a `last_keyset` (the `(created_at, id)` tuple of the final row of the page) when more pages remain, and omits it on the final page. The cursor wire format is NEVER forwarded to the plugin SPI; the plugin is opaque to the OData/cursor encoding. On host-resolution `PluginUnavailable` / plugin-side `Transient` / `Internal`, a fail-closed canonical `Problem` envelope per `usage-collector-v1.yaml`.

**Steps**:

1. [ ] - `p1` - Assemble the Plugin SPI `list_usage_records` request as the structured tuple `(gts_id: UsageTypeGtsId, filter_ast: FilterNode<UsageRecordFilterField>, order_keys: OrderKeys, page_after: Option<Keyset>, limit: u32)` per the contract published in `plugin-spi.md` Method 4 (the bounded `created_at` window rides `filter_ast`) (NEVER include the cursor wire format; the plugin is opaque to OData/cursor encoding) - `inst-raw-dispatch-assemble`
2. [ ] - `p1` - **TRY** invoke the storage-plugin `list_usage_records` capability via `cpt-cf-usage-collector-component-plugin-host` over the persisted usage records — the composed `FilterNode<UsageRecordFilterField>` AST is authoritative; the plugin MUST honor every predicate without widening and MUST emit rows in the requested `OrderKeys` projection - `inst-raw-dispatch-try`
3. [ ] - `p1` - **CATCH** Plugin SPI transport, readiness, or contract error (host-resolution `PluginUnavailable`, plugin-side `Transient`, or `Internal` per `plugin-spi.md` Method 4) - `inst-raw-dispatch-catch`
   1. [ ] - `p1` - **RETURN** the fail-closed canonical `Problem` envelope per `usage-collector-v1.yaml` (no synthesized partial page) - `inst-raw-dispatch-catch-return`
4. [ ] - `p1` - **RETURN** the `(rows: Vec<UsageRecord>, last_keyset: Option<Keyset>)` tuple to the gateway for cursor minting and envelope assembly (with an empty `rows` list and `last_keyset = None` when no rows match within the authorized scope — not an error per `plugin-spi.md` Method 4) - `inst-raw-dispatch-return`

### Cursor Pagination Orchestration

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-algo-usage-query-cursor-pagination-orchestration-v2`

**Input**: The optional `cursor` query parameter from `GET /usage-collector/v1/records` (toolkit `CursorV1` opaque token, base64url-encoded), the parsed `$filter` AST (`FilterNode<UsageRecordFilterField>`), the parsed `$orderby` projection (`OrderKeys` over the canonical keyset `(created_at, id)`), the bounded `limit` limit (≤ 1,000; over-cap rejected, not clamped), and the `(rows, last_keyset)` tuple returned by `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2`.

**Output**: A two-phase cursor-pagination decision owned end-to-end by the gateway. Phase 1 (decode + validate): the gateway decodes the inbound `cursor` query parameter as a toolkit `CursorV1` value, then calls `toolkit_odata::validate_cursor_against($filter, $orderby)` to ensure the cursor was minted against the exact same parsed filter AST and order-key projection as the current request. Every cursor failure lifts to `InvalidArgument` with a `field_violations[0]` on `cursor` and transitions to `rejected-validation`: malformed → `.reason="INVALID_CURSOR"`, `OrderMismatch` → `"ORDER_MISMATCH"`, `FilterMismatch` → `"FILTER_MISMATCH"`; the gateway also enforces the semantic mandatoriness of `timestamp ge X and timestamp lt Y` after OData parsing and before plugin dispatch and rejects a missing time-range window as `InvalidArgument` (`field_violations[0].field="$filter"`, `.reason="MISSING_TIME_WINDOW"`). On success the gateway projects the cursor to the plugin keyset `(created_at, id)` and forwards a typed `page_after: Option<Keyset>` to the plugin SPI. Phase 2 (mint + emit): on a successful page return, the gateway mints the next `CursorV1` from the plugin-returned `last_keyset` (bound to the current `$filter` AST and `$orderby` projection) and embeds it in the `toolkit_odata::Page<UsageRecord>` `@nextLink` URL; when `last_keyset = None`, the gateway omits `@nextLink` to signal the last page. The cursor is NEVER forwarded verbatim to the plugin SPI — the plugin is opaque to the OData/cursor wire encoding.

**Steps**:

1. [ ] - `p1` - **IF** the request carries a `cursor` query parameter - `inst-cursor-orchestration-incoming-check`
   1. [ ] - `p1` - Decode the `cursor` as a toolkit `CursorV1` value; on a malformed payload or version-tag mismatch **RETURN** the canonical `InvalidArgument` `Problem` (`field_violations[0].field="cursor"`, `.reason="INVALID_CURSOR"`) per `usage-collector-v1.yaml` without any plugin dispatch - `inst-cursor-orchestration-decode`
   2. [ ] - `p1` - Invoke `toolkit_odata::validate_cursor_against($filter, $orderby)` to confirm the cursor was minted against the exact same parsed filter AST and order-key projection as the current request; on `OrderMismatch` **RETURN** `InvalidArgument` (`field_violations[0].field="cursor"`, `.reason="ORDER_MISMATCH"`); on `FilterMismatch` **RETURN** `InvalidArgument` (`.reason="FILTER_MISMATCH"`); no plugin dispatch in either branch - `inst-cursor-orchestration-validate`
   3. [ ] - `p1` - Project the validated cursor to the plugin keyset `(created_at, id)` as a typed `page_after: Option<Keyset>` value - `inst-cursor-orchestration-project`
2. [ ] - `p1` - **ELSE** - `inst-cursor-orchestration-no-cursor-branch`
   1. [ ] - `p1` - Dispatch with `page_after = None`, so the plugin starts from the first page of the authorized scope per `plugin-spi.md` Method 4 - `inst-cursor-orchestration-first-page`
3. [ ] - `p1` - Forward the typed `page_after` (or `None`) to `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2` (the cursor wire format is NEVER forwarded to the plugin SPI) and receive the `(rows, last_keyset)` tuple - `inst-cursor-orchestration-dispatch`
4. [ ] - `p1` - **IF** the plugin returned a `last_keyset` (`Some(Keyset)`) - `inst-cursor-orchestration-next-check`
   1. [ ] - `p1` - Mint the next `CursorV1` from `last_keyset` bound to the current parsed `$filter` AST and `$orderby` projection per the toolkit-odata `CursorV1` contract; embed the minted cursor token in the `toolkit_odata::Page<UsageRecord>` `@nextLink` URL so the next caller forwards it back into the same request shape (cursor is gateway-owned state minted on each page; cross-binding cursors are rejected by `toolkit_odata::validate_cursor_against` as `FilterMismatch` / `OrderMismatch` on the next call) - `inst-cursor-orchestration-mint-next`
5. [ ] - `p1` - **ELSE** (`last_keyset = None`) - `inst-cursor-orchestration-no-next-branch`
   1. [ ] - `p1` - Omit `@nextLink` from the response — the plugin signaled the last page per `plugin-spi.md` Method 4 - `inst-cursor-orchestration-omit-next`
6. [ ] - `p1` - **RETURN** the `toolkit_odata::Page<UsageRecord>` envelope (`items`, optional `@nextLink` containing the freshly minted gateway-owned `CursorV1`) - `inst-cursor-orchestration-return`

### Active & Inactive Record Visibility

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-algo-usage-query-active-and-inactive-record-visibility`

**Input**: The PDP-authorized scope (the composed filter set from `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2` anchored on the resolved `SecurityContext`) and the candidate record set returned by the storage plugin (`cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2` for the aggregated path or `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2` for the raw path) over the persisted usage records.

**Output**: A visible row set in which both `active` and `inactive` rows within the PDP-authorized scope are returned (raw path: each `UsageRecord` carries its `status` field per `plugin-spi.md` Method 4 and `usage-collector-v1.yaml`; aggregated path: both `active` and `inactive` rows contribute to the `AggregationResult` `buckets`). An empty match within the authorized scope returns an empty result set / page — never a `Problem` envelope. Deactivation of `active` → `inactive` is owned by §2.5 Event Deactivation and is NOT performed here.

**Steps**:

1. [ ] - `p1` - Receive the candidate row set returned by the storage plugin from `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2` or `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2` - `inst-visibility-receive`
2. [ ] - `p1` - **FOR EACH** row in the candidate row set - `inst-visibility-iterate`
   1. [ ] - `p1` - Include the row when its lifecycle state is `active` OR `inactive` within the PDP-authorized scope (auditable history is preserved by surfacing both states; deactivation transitions remain owned by §2.5 Event Deactivation and are NOT performed in this feature) - `inst-visibility-include`
3. [ ] - `p1` - **IF** the visible row set is empty (no `active` or `inactive` rows matched within the PDP-authorized scope) - `inst-visibility-empty-check`
   1. [ ] - `p1` - **RETURN** an empty visible row set so the calling flow surfaces an empty `AggregationResult` `buckets` list or an empty `toolkit_odata::Page<UsageRecord>` `items` list per `usage-collector-v1.yaml` — empty match within the authorized scope is never a `Problem` envelope per `plugin-spi.md` Method 3 and Method 4 - `inst-visibility-empty-return`
4. [ ] - `p1` - **RETURN** the visible row set (both `active` and `inactive` rows within the PDP-authorized scope) to the calling flow for downstream assembly - `inst-visibility-return`

## 4. States (CDSL)

### Query Request Lifecycle State Machine

- [ ] `p2` - **ID**: `cpt-cf-usage-collector-state-usage-query-query-request-lifecycle`

**States**: `received`, `ctx-accepted`, `pdp-authorized`, `filter-validated`, `plugin-dispatched`, `result-returned`, `rejected-validation`, `denied`, `unavailable`

**Initial State**: `received`

**Final States**: `result-returned`, `rejected-validation`, `denied`, `unavailable`

**Transitions**:

1. [ ] - `p1` - **FROM** `received` **TO** `ctx-accepted` **WHEN** the inbound `SecurityContext` is present at the `cpt-cf-usage-collector-component-query-gateway` boundary — on REST as `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`) for the inbound aggregated read or the inbound raw read, on SDK as the `ctx: &SecurityContext` first argument to `UsageCollectorClientV1::query_aggregated_usage_records(ctx, ...)` or `UsageCollectorClientV1::list_usage_records(ctx, ...)` per `sdk-trait.md` Methods 3 and 4 — and the gateway proceeds with the read (the calling flow already exited fail-closed via `inst-aggregated-missing-ctx` / `inst-raw-missing-ctx` if the SecurityContext was absent) - `inst-state-query-ctx-accepted`
2. [ ] - `p1` - **FROM** `ctx-accepted` **TO** `pdp-authorized` **WHEN** `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (invoked via the per-component `access_scope_with` helper inside `cpt-cf-usage-collector-component-query-gateway` against `cpt-cf-usage-collector-contract-authz-resolver`) returns a `permit` `PdpDecision` paired with a non-empty `PdpConstraint` set, and `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read` binds both to the request payload (mirrors `inst-aggregated-pdp-delegate` + `inst-aggregated-attribution` and `inst-raw-pdp-delegate` + `inst-raw-attribution`) - `inst-state-query-pdp-authorized`
3. [ ] - `p1` - **FROM** `pdp-authorized` **TO** `filter-validated` **WHEN** the request passes structural OData parsing and post-parse validation AND — for the aggregated path only — the mandatory `aggregation` operator is present and supported (closed enum, validated at body deserialization) AND the typed `gts_id` is well-formed AND the mandatory bounded time window is present (`require_bounded_time_window`) AND the `gts_id`'s existence is resolved by a pre-dispatch `get_usage_type` call — an unregistered one surfacing as `NotFound` (404) before dispatch — and the resolved usage `kind` gates a pre-dispatch op-kind check that rejects a mismatched `(op, kind)` pair (`SUM` on a gauge, or `MIN` / `MAX` / `AVG` on a counter) with `InvalidArgument` (`400`, `.reason="OP_NOT_ALLOWED_FOR_KIND"`; counter admits `{SUM, COUNT}`, gauge admits `{MIN, MAX, AVG, COUNT}`) AND — for the raw path only — `limit` is within the bounded cap declared in `usage-collector-v1.yaml` (an absent `limit` defaulted to the cap, a present `limit` above it already rejected before this transition) AND the parsed `$filter` AST contains the mandatory `timestamp ge X and timestamp lt Y` window (semantic mandatoriness enforced by the usage-query algorithm at the gateway after OData parsing, NOT by toolkit-odata) AND the optional `cursor` (when present) decoded as toolkit `CursorV1` AND was validated by `toolkit_odata::validate_cursor_against($filter, $orderby)` via `cpt-cf-usage-collector-algo-usage-query-cursor-pagination-orchestration-v2` AND `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2` has AND-merged the PDP constraint set into the parsed `FilterNode<UsageRecordFilterField>` under intersection-only semantics (mirrors `inst-aggregated-structural-check`, `inst-aggregated-time-window-check`, `inst-aggregated-existence-and-op-kind-check`, `inst-aggregated-constraint-composition`, `inst-raw-odata-parse`, `inst-raw-time-range-mandatory-check`, `inst-raw-cursor-validate`, and `inst-raw-constraint-composition`) - `inst-state-query-filter-validated`
4. [ ] - `p1` - **FROM** `filter-validated` **TO** `plugin-dispatched` **WHEN** the Plugin SPI capability accepts the composed request — `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2` invokes `query_aggregated_usage_records` for the aggregated path (mirrors `inst-aggregated-plugin-dispatch`) or `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2` invokes `list_usage_records` for the raw path with the structured tuple `(filter_ast, order_keys, page_after, limit)` projected by `cpt-cf-usage-collector-algo-usage-query-cursor-pagination-orchestration-v2` (mirrors `inst-raw-plugin-dispatch`; the cursor wire format is NEVER forwarded to the plugin SPI) — and returns the candidate row set per `plugin-spi.md` Method 3 and Method 4 - `inst-state-query-plugin-dispatched`
5. [ ] - `p1` - **FROM** `plugin-dispatched` **TO** `result-returned` **WHEN** `cpt-cf-usage-collector-algo-usage-query-active-and-inactive-record-visibility` confirms both `active` and `inactive` rows within the PDP-authorized scope contribute to the result AND the gateway assembles either the `AggregationResult` (mirrors `inst-aggregated-visibility-rule` → `inst-aggregated-result-assemble` → `inst-aggregated-return`) or the `toolkit_odata::Page<UsageRecord>` envelope (mirrors `inst-raw-visibility-rule` → `inst-raw-page-assemble` → `inst-raw-return`); an empty match within the authorized scope still transitions here and returns an empty `buckets` list or an empty `items` list with no `next_cursor` — never a `Problem` envelope per `plugin-spi.md` Method 3 and Method 4; the `inst-aggregated-result-rows-observe` / `inst-raw-result-rows-observe` and `inst-aggregated-telemetry-complete` / `inst-raw-telemetry-complete` steps now interleaved into those flow chains emit operational metrics (per DESIGN §3.11.5) as a side-effect of reaching this state and are orthogonal to the transition itself — they neither gate it nor introduce a new state, so the mirror chains above track only the state-bearing steps - `inst-state-query-result-returned`
6. [ ] - `p1` - **FROM** `pdp-authorized` **TO** `rejected-validation` **WHEN** the request fails structural pre-checks after attribution binding — the aggregated path's mandatory bounded time window missing (mirrors `inst-aggregated-time-window-check`; → `MISSING_TIME_WINDOW`), the aggregated path's mandatory `aggregation` operator missing or unsupported (closed enum, rejected at body deserialization; mirrors `inst-aggregated-structural-check`), the aggregated path's `(op, kind)` pair not admitted by the resolved usage `kind` (`SUM` on a gauge, or `MIN` / `MAX` / `AVG` on a counter — rejected by the pre-dispatch op-kind check as `field_violations[0].reason="OP_NOT_ALLOWED_FOR_KIND"` on `aggregation.op`; mirrors `inst-aggregated-existence-and-op-kind-check`) — an unregistered `gts_id` is a separate pre-dispatch outcome, canonical `NotFound` (404) from the `get_usage_type` resolution rather than a `rejected-validation` transition — OR — for the raw path only — the OData `$filter` / `$orderby` / `limit` strings fail to parse (mirrors `inst-raw-odata-parse`), the parsed `$filter` AST is missing the mandatory `timestamp ge X and timestamp lt Y` window (mirrors `inst-raw-time-range-mandatory-check` — surfaced as `field_violations[0].reason="MISSING_TIME_WINDOW"` on `$filter`), or the optional `cursor` query parameter fails decode (`INVALID_CURSOR`) / `OrderMismatch` (`ORDER_MISMATCH`) / `FilterMismatch` (`FILTER_MISMATCH`), each a `field_violations[0]` on `cursor`, when validated by `toolkit_odata::validate_cursor_against` (mirrors `inst-raw-cursor-validate`); the gateway surfaces the canonical `toolkit_canonical_errors::Problem` `InvalidArgument` envelope (HTTP `400`) — the aggregated path's mandatory-aggregation-operator check (closed enum at body deserialization), its single-UsageType existence resolution (pre-dispatch `get_usage_type` → canonical `NotFound`, 404), and its op-kind check (pre-dispatch → `OP_NOT_ALLOWED_FOR_KIND`, 400) are all enforced before any Plugin SPI aggregate dispatch — and no Plugin SPI dispatch occurs for the realized raw-path checks (structural pre-checks run after PDP delegation per the flow ordering — there is no path from `received` directly to `rejected-validation`); a present `limit` above the bounded cap is rejected gateway-side in `prepare_list_query` with the canonical `toolkit_canonical_errors::Problem` envelope (`Problem.type` = `InvalidArgument`, HTTP `400`) carrying a `field_violations[0]` on `limit` with `.reason="VALIDATION"` (the gear handler produces this before any plugin dispatch — it is NOT clamped; an absent `limit` instead defaults to the 1,000 cap) - `inst-state-query-rejected-validation`
7. [ ] - `p1` - **FROM** `pdp-authorized` **TO** `rejected-validation` **WHEN** the inbound `cursor` query parameter on the raw path fails one of the three toolkit-cursor validation gates — `Malformed` (`field_violations[0].reason="INVALID_CURSOR"`), `OrderMismatch` (`"ORDER_MISMATCH"`), or `FilterMismatch` (`"FILTER_MISMATCH"`), each on the `cursor` field — and the toolkit `CursorV1` adoption DoD; the gateway rejects the request with the canonical `toolkit_canonical_errors::Problem` envelope before any Plugin SPI dispatch (cursor decode + validate is gateway-owned; the plugin SPI is opaque to the OData/cursor wire format and never receives an invalid cursor) - `inst-state-query-rejected-validation-cursor`
8. [ ] - `p1` - **FROM** `ctx-accepted` **TO** `denied` **WHEN** `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (invoked via the per-component `access_scope_with` helper inside `cpt-cf-usage-collector-component-query-gateway` against `cpt-cf-usage-collector-contract-authz-resolver`) returns a `PdpDecision` of `deny` (mirrors `inst-aggregated-pdp-deny-branch` → `inst-aggregated-pdp-deny-return` and `inst-raw-pdp-deny-branch` → `inst-raw-pdp-deny-return`); the gateway surfaces the propagated platform-authorization `Problem` envelope (`context.reason="AUTHZ"`) per `usage-collector-v1.yaml` without any plugin dispatch and never caches the decision - `inst-state-query-denied`
9. [ ] - `p1` - **FROM** `ctx-accepted` **TO** `denied` **WHEN** `cpt-cf-usage-collector-flow-foundation-pdp-authorize` returned `permit` but the `PdpConstraint` set is missing or empty (denying every row in the authorized scope) — `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read` short-circuits before binding (i.e. before `pdp-authorized` is reached, since transition 2 requires a non-empty `PdpConstraint` set to enter `pdp-authorized`) and surfaces this as the same fail-closed `Problem` envelope (`context.reason="AUTHZ"`), with no synthesized identity and no inferred result (mirrors `inst-attribution-fail-closed-check` → `inst-attribution-fail-closed-return` inside `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read`) - `inst-state-query-denied-empty-constraints`
10. [ ] - `p1` - **FROM** `received` **TO** `unavailable` **WHEN** the inbound `SecurityContext` is absent at the handler boundary — on REST the ToolKit gateway middleware did not populate `Extension<SecurityContext>` (mirrors `inst-aggregated-missing-ctx` and `inst-raw-missing-ctx`); on SDK the trait method was invoked without a `ctx` argument; the gateway surfaces the canonical `Unauthenticated` `Problem` envelope per `usage-collector-v1.yaml` without any further processing and never synthesizes identity - `inst-state-query-unavailable-missing-ctx`
11. [ ] - `p1` - **FROM** `ctx-accepted` **TO** `unavailable` **WHEN** `cpt-cf-usage-collector-flow-foundation-pdp-authorize` is unreachable so neither a `permit` nor a `deny` `PdpDecision` is available; `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read` discriminates this as the substrate-unreachable branch and returns the fail-closed `Problem` envelope routed to the `unavailable` state (mirrors `inst-attribution-fail-closed-check` → `inst-attribution-fail-closed-substrate-return`) with no cached decision - `inst-state-query-unavailable-pdp`
12. [ ] - `p1` - **FROM** `filter-validated` **TO** `unavailable` **WHEN** `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2` or `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2` surfaces a Plugin SPI transport / readiness / contract error (host-resolution `PluginUnavailable`, plugin-side `Transient`, or `Internal` per `plugin-spi.md` Method 3 and Method 4; mirrors `inst-aggregated-plugin-catch` → `inst-aggregated-plugin-catch-return` and `inst-raw-plugin-catch` → `inst-raw-plugin-catch-return`); the gateway surfaces the fail-closed `Problem` envelope per `usage-collector-v1.yaml`, never synthesizes a partial aggregation / page, and never caches a prior decision - `inst-state-query-unavailable-plugin`

## 5. Definitions of Done

### FR: Aggregation Rule — SUM Nets, Others Usage Only

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-aggregation-sum-nets`

The system **MUST** surface the locked aggregation contract on the read path so downstream consumers can reason about counter totals with compensation applied: `SUM(value)` aggregates across active rows regardless of `corrects_id` presence, treating `value` as a signed quantity, so `SUM` is the **signed net total** per group (rows with `corrects_id IS NOT NULL` carry a strictly-negative `value` and reduce the running counter total); `COUNT`, `MIN`, `MAX`, and `AVG` filter to active rows WHERE `corrects_id IS NULL` before aggregating. Aggregation ops are restricted per usage `kind` — counter admits `{SUM, COUNT}`, gauge admits `{MIN, MAX, AVG, COUNT}` — and a mismatched `(op, kind)` pair is rejected at the gateway with `InvalidArgument` (`.reason="OP_NOT_ALLOWED_FOR_KIND"`) before plugin dispatch. **Compensation entries adjust SUM; they are not events.** Counting a compensation as an event would double-count the original usage event (the row its `corrects_id` references is already counted); including a strictly-negative compensation `value` in `MIN` / `MAX` / `AVG` would corrupt extremes and means. Status filtering applies before aggregation — deactivated rows are excluded from every aggregation regardless of `corrects_id` presence; the `active`-status filter and the `corrects_id`-presence filter are orthogonal. A negative `SUM(value)` is an ordinary aggregation outcome — the Usage Collector does NOT validate non-negative net and does NOT emit negative-net detection per the un-policed-net stance recorded in `cpt-cf-usage-collector-adr-usage-compensation`; downstream consumers (billing, quota, FinOps) own any "net can't be negative" policy. The rule is encoded in `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2` and honoured server-side by the storage plugin per `plugin-spi.md` Method 3.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2`

**Constraints**: `cpt-cf-usage-collector-fr-usage-compensation`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`
- Entities: `AggregationQuery`, `AggregationResult`, `UsageRecord`

### FR: Query Aggregation

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-fr-query-aggregation`

The system **MUST** expose `POST /usage-collector/v1/records/aggregate` (and the SDK `query_aggregated_usage_records` operation per `sdk-trait.md`) as the single contract-first aggregated read path, accept an `AggregationRequest` carrying a mandatory `time_range`, a mandatory single-UsageType filter (`gts_id`), a mandatory `aggregation` operator (`SUM` / `COUNT` / `MIN` / `MAX` / `AVG` per the `AggregationOp` enum in `usage-collector-v1.yaml`), and optional narrowing filters / `group_by` keys per `usage-collector-v1.yaml`, route every submission through `cpt-cf-usage-collector-component-query-gateway`, and end the synchronous path with a server-side aggregation executed by the storage plugin through the Plugin SPI `query_aggregated_usage_records` capability over the persisted usage records — surfacing a `AggregationResult` (`gts_id`, `aggregation`, `buckets`) anchored on the PDP-narrowed scope of the resolved `SecurityContext`, with an empty `buckets` list when no rows match (never a `Problem` envelope). Aggregation ops are restricted per usage `kind` — counter admits `{SUM, COUNT}`, gauge admits `{MIN, MAX, AVG, COUNT}` — and a mismatched `(op, kind)` pair is rejected at the gateway with `InvalidArgument` (`.reason="OP_NOT_ALLOWED_FOR_KIND"`) before plugin dispatch. The `corrects_id`-aware aggregation contract — `SUM` nets signed values across active rows regardless of `corrects_id` presence; `COUNT`/`MIN`/`MAX`/`AVG` over active rows WHERE `corrects_id IS NULL` ("compensation entries adjust SUM; they are not events") — is governed by `cpt-cf-usage-collector-dod-usage-query-aggregation-sum-nets`.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2`
- `cpt-cf-usage-collector-seq-query-aggregated`

**Constraints**: `cpt-cf-usage-collector-fr-query-aggregation`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`
- Entities: `AggregationQuery`, `AggregationResult`

### FR: Query Raw

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-fr-query-raw`

The system **MUST** expose `GET /usage-collector/v1/records` with OData query parameters (`$filter`, `$orderby`, `limit`, `cursor`) — and the SDK `list_usage_records` operation per `sdk-trait.md` — as the single contract-first raw read path; accept `$filter` as an OData predicate over `UsageRecordFilterField` that MUST include the mandatory `timestamp ge X and timestamp lt Y` window (semantic mandatoriness enforced at the gateway after OData parsing and before plugin dispatch; missing window → canonical `InvalidArgument` `Problem`, `field_violations[0].reason="MISSING_TIME_WINDOW"` on `$filter`), `$orderby` is optional and normalized gateway-side to end in the canonical unique `(created_at, id)` suffix (appended in the caller order's direction; an omitted `$orderby` becomes `created_at asc, id asc`), while a caller order mixing `asc` and `desc` or naming an optional (nullable) field is rejected `400` with `field_violations[0].reason="VALIDATION"` on `$orderby` because keyset pagination requires a uniform-direction, never-null keyset, `limit` is bounded at 1,000 records per page (absent → defaults to the cap; a present value over the cap → rejected gateway-side with `field_violations[0].reason="VALIDATION"` on `limit`, never clamped), and `cursor` is a toolkit `CursorV1` opaque token decoded and validated at the gateway via `toolkit_odata::validate_cursor_against` (decode failure → `INVALID_CURSOR`; cursor minted against a different `$filter` → `FILTER_MISMATCH`; against a different `$orderby` → `ORDER_MISMATCH`; each a `field_violations[0]` on `cursor`); route every submission through `cpt-cf-usage-collector-component-query-gateway` and end the synchronous path with a cursor-paginated page returned by the storage plugin through the Plugin SPI `list_usage_records` capability invoked with the structured tuple `(filter_ast: FilterNode<UsageRecordFilterField>, order_keys: OrderKeys, page_after: Option<Keyset>, limit: u32)` over the persisted usage records — surfacing a `toolkit_odata::Page<UsageRecord>` envelope (`items`, optional `@nextLink` containing the freshly minted gateway-owned `CursorV1` bound to the current `$filter` AST and `$orderby` projection) anchored on the PDP-narrowed scope, with an empty `items` list (and no `@nextLink`) when no rows match (never a `Problem` envelope) and a freshly minted `CursorV1` in `@nextLink` only when more pages remain.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-raw`
- `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2`
- `cpt-cf-usage-collector-algo-usage-query-cursor-pagination-orchestration-v2`
- `cpt-cf-usage-collector-seq-query-raw`

**Constraints**: `cpt-cf-usage-collector-fr-query-raw`

**Touches**:

- API: `GET /usage-collector/v1/records`
- Entities: `RawQuery`, `UsageRecordFilterField`, `Keyset`, `toolkit_odata::Page<UsageRecord>`, `CursorV1`

### FR: Tenant Isolation

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-fr-tenant-isolation`

The system **MUST** derive tenant scope on every aggregated and raw read solely from the inbound `SecurityContext` and the `PdpConstraint` set returned by `cpt-cf-usage-collector-flow-foundation-pdp-authorize` through the per-component `access_scope_with` helper inside `cpt-cf-usage-collector-component-query-gateway` (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`), refuse any caller-supplied filter that attempts to widen the authorized tenant scope (clamped silently to the PDP bound — no widening permitted under any user-supplied input), and never return cross-tenant rows absent an explicit platform PDP permit —.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-flow-usage-query-query-raw`
- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read`
- `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2`

**Constraints**: `cpt-cf-usage-collector-principle-pdp-centric-authorization`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`, `GET /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-query-gateway`
- Entities: `SecurityContext`, `PdpConstraint`

### NFR: Query Latency

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-nfr-query-latency`

The system **MUST** meet the `cpt-cf-usage-collector-nfr-query-latency` budget on both read paths — p95 latency under the documented canonical load envelope (30-day single-tenant aggregated query bracketed by the `uc_query_duration_seconds` histogram per DESIGN §3.11) — by pushing aggregation and pagination into the storage plugin via the Plugin SPI (no per-row fan-out into the core per `plugin-spi.md` Method 3 and Method 4), gating every read with the per-component `access_scope_with` helper invocation against `cpt-cf-usage-collector-contract-authz-resolver` on the critical path without a results cache, and surfacing query timing metrics for SLO monitoring.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-flow-usage-query-query-raw`
- `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2`
- `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2`

**Constraints**: `cpt-cf-usage-collector-nfr-query-latency`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`, `GET /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-query-gateway`

### NFR: Workload Isolation

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-nfr-workload-isolation`

The system **MUST** isolate the read workload from the ingestion workload — `cpt-cf-usage-collector-component-query-gateway` is the only read-side dispatch component and remains structurally separate from `cpt-cf-usage-collector-component-ingestion-gateway`, so a read-side load spike or plugin slowdown MUST NOT degrade ingestion throughput; query in-flight and outcome telemetry (`uc_query_inflight`, `uc_query_requests_total` per DESIGN §3.11) are surfaced separately from the ingestion telemetry families.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-flow-usage-query-query-raw`

**Constraints**: `cpt-cf-usage-collector-nfr-workload-isolation`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`, `GET /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-query-gateway`

### NFR: Operational Visibility (Query-Path Instruments)

- [x] `p2` - **ID**: `cpt-cf-usage-collector-dod-usage-query-nfr-operational-visibility`

The system **MUST** emit the four query-path operational instruments owned by `cpt-cf-usage-collector-component-query-gateway` per the authoritative inventory rows in DESIGN [§3.11.5](../DESIGN.md#3115-operational-metric-inventory-ops-design-002), constructed on the gear's scoped `Meter` and pushed via OTLP through ToolKit's `SdkMeterProvider` per DESIGN [§3.11.4](../DESIGN.md#3114-observability-architecture-applicability-ops-design-002) (no gear-local `/metrics` scrape endpoint) — instrument names, label vocabularies, and bucket layouts are owned by the inventory and cited here, not redefined — realized on both read paths (REST and SDK alike) at the flow emit-point steps `inst-aggregated-inflight-increment` / `inst-raw-inflight-increment`, `inst-aggregated-result-rows-observe` / `inst-raw-result-rows-observe`, and `inst-aggregated-telemetry-complete` / `inst-raw-telemetry-complete`. **This entire surface is specified but not yet wired in gear source** (no meter instrument exists today — see §1.6 Deferred):

- `uc_query_requests_total` (counter, labels `query_kind` / `outcome` / `error_category`) **MUST** be incremented exactly once when every query attempt completes — success or failure, aggregated and raw alike — carrying the `(query_kind, outcome, error_category)` tuple projected by the feature-owned exit→category mapping in `inst-aggregated-telemetry-complete` / `inst-raw-telemetry-complete`, where `error_category="none"` **MUST** be emitted only when `outcome="success"`. The label value sets are the closed DESIGN [§3.11.5](../DESIGN.md#3115-operational-metric-inventory-ops-design-002) `uc_query_requests_total` vocabulary and are cited, not restated, here; every emitted label **MUST** stay within those value sets, which align with the canonical `Problem` discriminators per `cpt-cf-usage-collector-principle-canonical-errors`. The closed-set REST-handler-boundary categories (`missing_security_context`, `cursor_decode`, `order_mismatch`, `filter_mismatch`) are part of this contract but are produced upstream of the service pipeline (cursor / boundary checks in the REST handler); recording them requires handler-boundary emission, deferred per §1.6. Generic request-shape rejections that carry the `VALIDATION` `field_violations[].reason` (`limit` over cap, unknown parameters, malformed `gts_id`, unparseable OData) have **no** member in the closed §3.11.5 `error_category` set and are therefore not recorded on the counter at all — mirroring the ingestion sibling's structural `inst-emit-batch-cap-check` rejection in `usage-emission.md`.
- `uc_query_duration_seconds` (histogram, label `query_kind`) **MUST** be observed exactly once when the same attempt completes, measuring the attempt's wall-clock seconds; the bucket layout brackets the 500 ms p95 `cpt-cf-usage-collector-nfr-query-latency` budget for the canonical 30-day single-tenant aggregated query per the DESIGN [§3.11.5](../DESIGN.md#3115-operational-metric-inventory-ops-design-002) histogram row and the latency-budget table in DESIGN [§3.11.2](../DESIGN.md#3112-latency-budgets-perf-design-003) — the layout is cited from the inventory, not redefined here; the latency-SLO obligation carried on this histogram stays with `cpt-cf-usage-collector-dod-usage-query-nfr-query-latency`, which this DoD references rather than re-owns.
- `uc_query_inflight` (gauge, label `query_kind`) **MUST** be incremented on query-gateway entry once authorization composes and decremented on query completion or failure — every exit path that follows the increment drains it, so the gauge never leaks under an early return; it is the current-state in-flight series feeding the workload-isolation alert in DESIGN [§3.11.6](../DESIGN.md#3116-alerting-and-error-budget-architecture-ops-design-005) and traces to `cpt-cf-usage-collector-nfr-workload-isolation`; the read-side surfacing obligation is shared with `cpt-cf-usage-collector-dod-usage-query-nfr-workload-isolation`, which names this gauge and the request counter and is referenced, not re-owned, here.
- `uc_query_result_rows` (histogram, label `query_kind`) **MUST** be recorded exactly once when a query completes successfully — the raw page size (`items` length, ≤ 1,000 by the `limit` bound) or the aggregated group count (`buckets` length, capped at 100,000 per `usage-collector-v1.yaml`) — so that, read together with `uc_query_duration_seconds`, operators separate "slow because large" from "slow because degraded" per the DESIGN [§3.11.5](../DESIGN.md#3115-operational-metric-inventory-ops-design-002) histogram row.

The PDP-shared instruments (`uc_pdp_failures_total`, `uc_pdp_duration_seconds`, `uc_authz_decisions_total`, `uc_pdp_ready`) are owned by the Foundation feature's shared `access_scope_with` helper and the plugin-host instruments (`uc_plugin_*`) by the Foundation-owned plugin host — the query gateway's PDP and Plugin SPI dispatch steps inherit them and this DoD does **NOT** respecify them. Unbounded identifiers (`tenant_id`, UsageType `gts_id`, `subject_id`, `resource_id`, `trace_id`, `request_id`, cursor tokens) **MUST NOT** appear as labels on any of the four instruments per the DESIGN [§3.11.5](../DESIGN.md#3115-operational-metric-inventory-ops-design-002) label-cardinality rule — they belong in structured logs and traces.

This DoD realizes the query-path share of `cpt-cf-usage-collector-nfr-operational-visibility` (the NFR itself is foundation-owned per DECOMPOSITION §2.1) and feeds the workload-isolation alert source in DESIGN [§3.11.6](../DESIGN.md#3116-alerting-and-error-budget-architecture-ops-design-005). The `error_category` values emitted are exactly the reachable, closed-set members projected by the exit→category mapping in `inst-aggregated-telemetry-complete` / `inst-raw-telemetry-complete` (the service-boundary members recorded now; the handler-boundary members — including `missing_security_context` — recorded once handler-boundary emission lands). No `error_category` outside the closed §3.11.5 set is ever emitted; the gear's generic `VALIDATION` request-shape rejections (which have no member in that set) are refused pre-pipeline and left unrecorded rather than mapped to an unrelated category.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-flow-usage-query-query-raw`

**Constraints**: `cpt-cf-usage-collector-nfr-operational-visibility`, `cpt-cf-usage-collector-nfr-workload-isolation`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`, `GET /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-query-gateway`
- Telemetry (specified; **not yet wired** in gear source — no meter instrument exists today, see §1.6): `uc_query_requests_total` counter, `uc_query_duration_seconds` histogram, `uc_query_inflight` gauge, `uc_query_result_rows` histogram

### NFR: Authorization

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-nfr-authorization`

The system **MUST** accept an inbound `SecurityContext` at both query entry points — on REST as `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`), on the SDK trait as `ctx: &SecurityContext` first parameter to `UsageCollectorClientV1::query_aggregated_usage_records` / `list_usage_records` per `sdk-trait.md` Methods 3 and 4 — and obtain the `(PdpDecision, PdpConstraint set)` envelope via `cpt-cf-usage-collector-flow-foundation-pdp-authorize` invoked through the per-component `access_scope_with` helper inside `cpt-cf-usage-collector-component-query-gateway` (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) on every aggregated and raw read before any Plugin SPI dispatch, never cache a prior PDP decision and never synthesize identity, and fail closed with the canonical `Unauthenticated` `Problem` envelope (missing `SecurityContext`) or the propagated platform-authorization `Problem` envelope (PDP unavailable or `deny`).

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read`

**Constraints**: `cpt-cf-usage-collector-principle-fail-closed`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`, `GET /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-query-gateway`

### Principle: PDP-Centric Authorization

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-principle-pdp-centric-authorization`

The system **MUST** flow every authorization decision — including row-scope narrowing — through `cpt-cf-usage-collector-flow-foundation-pdp-authorize` invoked via the per-component `access_scope_with` helper inside `cpt-cf-usage-collector-component-query-gateway` (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`), MUST NOT inline any authorization logic outside the helper-bound invocation site, MUST compose user-supplied filters with the returned `PdpConstraint` set under intersection-only semantics, and MUST NOT widen the PDP-authorized scope under any user-supplied input (any widening attempt is silently clamped back to the PDP bound — no widening permitted under any circumstance).

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read`
- `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2`

**Constraints**: `cpt-cf-usage-collector-principle-pdp-centric-authorization`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`, `GET /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-query-gateway`
- Entities: `PdpConstraint`, `SecurityContext`

### Principle: Fail-Closed

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-principle-fail-closed`

The system **MUST** return the canonical `Unauthenticated` `Problem` envelope when the inbound `SecurityContext` is missing at the handler boundary (REST handler did not receive `Extension<SecurityContext>` from ToolKit gateway middleware, or the SDK trait was invoked without a `ctx` argument); and the `unavailable` outcome with the fail-closed `Problem` envelope per `usage-collector-v1.yaml` when PDP (`cpt-cf-usage-collector-flow-foundation-pdp-authorize` invoked via the per-component `access_scope_with` helper inside `cpt-cf-usage-collector-component-query-gateway` against `cpt-cf-usage-collector-contract-authz-resolver`) or the bound storage plugin (Plugin SPI host-resolution `PluginUnavailable` / plugin-side `Transient` / `Internal` per `plugin-spi.md` Method 3 and Method 4) is unreachable on either read path; the gateway MUST NOT synthesize a partial aggregation, MUST NOT synthesize a partial page, MUST NOT cache a prior PDP decision, and MUST NOT infer identity.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-flow-usage-query-query-raw`
- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read`

**Constraints**: `cpt-cf-usage-collector-principle-fail-closed`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`, `GET /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-query-gateway`

### Constraint: No Business Logic

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-constraint-no-business-logic`

The system **MUST** keep `cpt-cf-usage-collector-component-query-gateway` free of pricing, rating, invoice-generation, quota-enforcement, and any other business-rule transformation — the read path surfaces raw `UsageRecord` rows (raw path) and counter / gauge `AggregationResult` `buckets` (aggregated path) verbatim from the storage plugin without unit conversion, currency conversion, or rule-based filtering; downstream rating / billing / reporting consumers own all such transformations.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-flow-usage-query-query-raw`

**Constraints**: `cpt-cf-usage-collector-constraint-no-business-logic`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`, `GET /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-query-gateway`

### Constraint: NFR Thresholds

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-constraint-nfr-thresholds`

The system **MUST** enforce every NFR threshold relevant to the read path at `cpt-cf-usage-collector-component-query-gateway` prior to Plugin SPI dispatch — mandatory `time_range`, the raw-path `page_size` cap (≤ 1,000 records per page), the aggregated-path result cap (≤ 100,000 rows over a 90-day single-tenant window with ≤ 2 groupings), and the query-latency budget (`cpt-cf-usage-collector-nfr-query-latency`) — surfacing a request-level structural validation `Problem` envelope on cap violation per `usage-collector-v1.yaml` and never relying on the storage plugin to enforce a missing gateway-side cap.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-flow-usage-query-query-raw`
- `cpt-cf-usage-collector-algo-usage-query-cursor-pagination-orchestration-v2`

**Constraints**: `cpt-cf-usage-collector-constraint-nfr-thresholds`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`, `GET /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-query-gateway`

### Component: Query Gateway

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-component-query-gateway`

The system **MUST** realize `cpt-cf-usage-collector-component-query-gateway` per DESIGN §3.2 Component Model — front the two read endpoints (`POST /usage-collector/v1/records/aggregate`, `GET /usage-collector/v1/records`) and the SDK read operations, accept the `SecurityContext` at both entry points (REST handler with `Extension<SecurityContext>` from ToolKit gateway middleware via `OperationBuilder::authenticated()`; SDK trait `query_aggregated_usage_records(ctx, ...)` / `list_usage_records(ctx, ...)` with `ctx: &SecurityContext` as the first parameter per `sdk-trait.md` Methods 3 and 4), perform structural validation (mandatory `time_range`, page-cap, aggregated-path single-UsageType filter), perform per-component PDP enforcement via the `access_scope_with` helper (`PolicyEnforcer::access_scope_with(ctx, ...)` against `cpt-cf-usage-collector-contract-authz-resolver`) realizing `cpt-cf-usage-collector-flow-foundation-pdp-authorize`, compose user-supplied filters with the returned `PdpConstraint` set under intersection-only semantics, dispatch to the bound storage plugin through `cpt-cf-usage-collector-component-plugin-host`, and serialize the result `AggregationResult` or `toolkit_odata::Page<UsageRecord>` per `usage-collector-v1.yaml` without inlining any business logic and without caching results.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-flow-usage-query-query-raw`

**Constraints**: `cpt-cf-usage-collector-component-query-gateway`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`, `GET /usage-collector/v1/records`
- Entities: `AggregationQuery`, `AggregationResult`, `RawQuery`, `UsageRecordFilterField`, `Keyset`, `toolkit_odata::Page<UsageRecord>`, `CursorV1`

### Sequence: Query Aggregated

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-seq-query-aggregated`

The system **MUST** implement `cpt-cf-usage-collector-seq-query-aggregated` end-to-end per DESIGN §3.6 — thread the caller through `cpt-cf-usage-collector-interface-rest-api` (REST handler receiving `Extension<SecurityContext>` from ToolKit gateway middleware) or `cpt-cf-usage-collector-interface-sdk-client` (SDK trait `query_aggregated_usage_records(ctx, ...)` with `ctx: &SecurityContext` first per `sdk-trait.md` Method 3), `cpt-cf-usage-collector-component-query-gateway` (which performs per-component PDP authorization via the `access_scope_with` helper against `cpt-cf-usage-collector-contract-authz-resolver` and enforces the mandatory bounded time window via `require_bounded_time_window`), `cpt-cf-usage-collector-component-plugin-host`, and the bound storage plugin (pure-persistence) — with the gateway resolving `gts_id` existence pre-dispatch via a `get_usage_type` call (an unregistered one surfacing as canonical `NotFound`, 404, before dispatch) and enforcing the op-per-kind restriction pre-dispatch (a mismatched `(op, kind)` → `InvalidArgument`, `.reason="OP_NOT_ALLOWED_FOR_KIND"`, 400; counter admits `{SUM, COUNT}`, gauge admits `{MIN, MAX, AVG, COUNT}`), and narrowing the user-supplied filters with the PDP-returned `PdpConstraint` set under intersection-only semantics prior to Plugin SPI `query_aggregated_usage_records` dispatch.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`

**Constraints**: `cpt-cf-usage-collector-seq-query-aggregated`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`
- Component: `cpt-cf-usage-collector-component-usage-type-catalog`, `cpt-cf-usage-collector-component-query-gateway`, `cpt-cf-usage-collector-component-plugin-host`

### Sequence: Query Raw

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-seq-query-raw`

The system **MUST** implement `cpt-cf-usage-collector-seq-query-raw` end-to-end per DESIGN §3.6 — thread the caller through `cpt-cf-usage-collector-interface-rest-api` (REST handler receiving `Extension<SecurityContext>` from ToolKit gateway middleware) or `cpt-cf-usage-collector-interface-sdk-client` (SDK trait `list_usage_records(ctx, ...)` with `ctx: &SecurityContext` first per `sdk-trait.md` Method 4), `cpt-cf-usage-collector-component-query-gateway` (which performs per-component PDP authorization via the `access_scope_with` helper against `cpt-cf-usage-collector-contract-authz-resolver`), `cpt-cf-usage-collector-component-plugin-host`, and the bound storage plugin — narrowing user-supplied predicates with the PDP-returned `PdpConstraint` set under intersection-only semantics, decoding and validating the optional toolkit `CursorV1` `cursor` query parameter against the parsed `$filter` AST and `$orderby` projection via `toolkit_odata::validate_cursor_against` at the gateway (the cursor wire format is NEVER forwarded to the plugin SPI; the gateway mints a fresh `CursorV1` from the plugin-returned `last_keyset` and embeds it in `@nextLink` when more pages remain), dispatching the structured tuple `(filter_ast, order_keys, page_after, limit)` to the Plugin SPI `list_usage_records` capability, and bounding `limit` prior to dispatch (absent → the 1,000 cap; a present value over the cap rejected gateway-side with `field_violations[0].reason="VALIDATION"`, never clamped).

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-raw`
- `cpt-cf-usage-collector-algo-usage-query-cursor-pagination-orchestration-v2`

**Constraints**: `cpt-cf-usage-collector-seq-query-raw`

**Touches**:

- API: `GET /usage-collector/v1/records`
- Entities: `CursorV1`, `Keyset`, `toolkit_odata::Page<UsageRecord>`


### Contract: Downstream Usage Reader

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-contract-downstream-usage-reader`

The system **MUST** honor the outbound `cpt-cf-usage-collector-contract-downstream-usage-reader` surface served by `cpt-cf-usage-collector-component-query-gateway` per DESIGN §3.5 Downstream Usage Reader Contract — downstream rating / billing / reporting / dashboard consumers depend on the documented REST and SDK request shapes (`AggregationQuery`, `RawQuery`), the documented result shapes (`AggregationResult`, `toolkit_odata::Page<UsageRecord>` for the raw read, toolkit `CursorV1` opaque continuation embedded in `@nextLink`), the PDP-narrowed scope semantics (filters can only narrow, never widen), the stable error categories (`InvalidArgument` with `field_violations[0].reason` ∈ {`MISSING_TIME_WINDOW`, `INVALID_CURSOR`, `ORDER_MISMATCH`, `FILTER_MISMATCH`, `INVALID_ORDERBY_FIELD`, and `VALIDATION` for a `limit` over the page cap}; `PermissionDenied`; `NotFound` for an unregistered usage type; `ServiceUnavailable` per `usage-collector-v1.yaml`), and the active-and-inactive record visibility rule. Business logic (pricing, rating, invoice generation, quota enforcement) MUST NOT be performed inside the Usage Collector — it is the responsibility of the downstream reader.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-flow-usage-query-query-raw`

**Constraints**: `cpt-cf-usage-collector-contract-downstream-usage-reader`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`, `GET /usage-collector/v1/records`
- Entities: `AggregationQuery`, `AggregationResult`, `RawQuery`, `UsageRecordFilterField`, `Keyset`, `toolkit_odata::Page<UsageRecord>`, `CursorV1`

### Entity: AggregationQuery

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-entity-aggregation-query`

The system **MUST** treat `AggregationQuery` per DESIGN §3.1 — accept exactly one mandatory `gts_id` as a typed required parameter (a malformed value → `InvalidArgument`, `.reason="INVALID_BASE_GTS_ID"`; an unregistered `gts_id` surfaces as `NotFound` from a pre-dispatch `get_usage_type` resolution, which also resolves the usage `kind` so an `(op, kind)` pair the kind does not admit (counter admits `{SUM, COUNT}`; gauge admits `{MIN, MAX, AVG, COUNT}`) is rejected as `InvalidArgument`, `.reason="OP_NOT_ALLOWED_FOR_KIND"`, before dispatch), one mandatory bounded time window (missing → `InvalidArgument`, `.reason="MISSING_TIME_WINDOW"`), a mandatory `aggregation` operator (`SUM` / `COUNT` / `MIN` / `MAX` / `AVG` per the `AggregationOp` enum in `usage-collector-v1.yaml`; a missing or unsupported value is rejected at request-body deserialization as `InvalidArgument`, HTTP `400`), optional `group_by` keys, and optional caller-supplied narrowing filters (`tenant_id` / `resource_ref` / `subject_ref` / `status` per `usage-collector-v1.yaml` `AggregationRequest`) that MUST NOT widen the PDP-authorized scope under any user-supplied input (clamped silently).

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`

**Constraints**: `AggregationQuery`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`
- Entities: `AggregationQuery`

### Entity: AggregationResult

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-entity-aggregation-result`

The system **MUST** treat `AggregationResult` per DESIGN §3.1 — return aggregated counter / gauge `buckets` for the resolved PDP-authorized scope (anchored on the `SecurityContext`), surface `gts_id`, the chosen `aggregation`, and the `buckets` list verbatim from the storage plugin without business-logic transformation, and surface an empty `buckets` list (never a `Problem` envelope) when no rows match within the authorized scope per `plugin-spi.md` Method 3.

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2`

**Constraints**: `AggregationResult`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`
- Entities: `AggregationResult`

### Entity: RawQuery

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-entity-raw-query`

The system **MUST** treat `RawQuery` per DESIGN §3.1 — accept the mandatory `timestamp ge X and timestamp lt Y` time-range window expressed inside the `$filter` OData predicate (semantic mandatoriness enforced at the gateway after OData parsing and before plugin dispatch), an optional `cursor` query parameter (toolkit `CursorV1` opaque token decoded and validated at the gateway via `toolkit_odata::validate_cursor_against`; never decoded by the plugin SPI), a bounded `limit` (≤ 1,000 records per page), an optional `$orderby` the gateway normalizes to end in the canonical unique `(created_at, id)` suffix, and optional caller-supplied narrowing predicates over `UsageRecordFilterField` (`tenant_id` / `subject_id` / `subject_type` / `resource_id` / `resource_type` / `status` per `usage-collector-v1.yaml`) that MUST NOT widen the PDP-authorized scope under any user-supplied input (clamped silently).

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-raw`
- `cpt-cf-usage-collector-algo-usage-query-cursor-pagination-orchestration-v2`

**Constraints**: `RawQuery`

**Touches**:

- API: `GET /usage-collector/v1/records`
- Entities: `RawQuery`

### Cursor: CursorV1 Toolkit Adoption

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-cursor-v1-toolkit-adoption`

The system **MUST** adopt toolkit `CursorV1` as the raw-read continuation wire format and locate cursor decode + validation at the gateway:

- Cursor wire format is toolkit `CursorV1` (opaque to client, base64url-encoded, contains version tag + bound filter/order digest + keyset payload).
- The gateway decodes and validates the cursor against the current parsed `$filter` AST and `$orderby` projection via `toolkit_odata::validate_cursor_against` BEFORE any PDP or plugin work.
- Validation failures map to canonical `InvalidArgument` `Problem` responses with a `field_violations[0]` on `cursor`: `Malformed` → `reason="INVALID_CURSOR"`; `OrderMismatch` → `"ORDER_MISMATCH"`; `FilterMismatch` → `"FILTER_MISMATCH"`.
- The cursor is bound to the canonical keyset `(created_at, id)` and is NEVER decoded by the plugin SPI; the plugin receives only a typed `page_after: Option<Keyset>` projected from the validated cursor by the gateway.
- Existing entity-cursor-token semantics (opaque, single-use, server-minted) are preserved; what changes is the wire format (toolkit `CursorV1`) and the validation locus (gateway, not plugin).

**Implements**:

- `cpt-cf-usage-collector-algo-usage-query-cursor-pagination-orchestration-v2`
- `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2`
- `cpt-cf-usage-collector-flow-usage-query-query-raw`

**Constraints**: `cpt-cf-usage-collector-principle-cursor-gateway-ownership`

**Touches**:

- API: `GET /usage-collector/v1/records`
- Entities: `CursorV1`, `Keyset`, `UsageRecordFilterField`

### Entity: PdpConstraint

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-entity-pdp-constraint`

The system **MUST** consume `PdpConstraint` per foundation DESIGN — a read-only constraint envelope returned by `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (invoked from `cpt-cf-usage-collector-component-query-gateway` via the per-component `access_scope_with` helper against `cpt-cf-usage-collector-contract-authz-resolver`) paired with the `PdpDecision`, composed with the user-supplied filters under intersection-only semantics such that user-supplied filters MUST NOT widen the authorized scope under any user-supplied input (any widening attempt is silently clamped back to the constraint bound).

**Implements**:

- `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2`
- `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read`
- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`

**Constraints**: `PdpConstraint`

**Touches**:

- Component: `cpt-cf-usage-collector-component-query-gateway`
- Entities: `PdpConstraint`

### Entity: SecurityContext

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-entity-security-context`

The system **MUST** consume `SecurityContext` per foundation DESIGN — the platform-resolved caller-identity envelope accepted at the two convention-bound entry points (on REST as `Extension<SecurityContext>` populated by ToolKit gateway middleware via `OperationBuilder::authenticated()`; on the SDK trait as `ctx: &SecurityContext` first parameter to `UsageCollectorClientV1::query_aggregated_usage_records(ctx, ...)` / `list_usage_records(ctx, ...)` per `sdk-trait.md` Methods 3 and 4) — as the SOLE source of tenant scope on both read paths; `cpt-cf-usage-collector-component-query-gateway` MUST anchor every PDP-constraint composition (via the per-component `access_scope_with` helper against `cpt-cf-usage-collector-contract-authz-resolver`) and every Plugin SPI dispatch on this inbound context, MUST NOT synthesize or infer identity, and MUST NOT widen the authorized tenant scope under any user-supplied filter.

**Implements**:

- `cpt-cf-usage-collector-flow-foundation-pdp-authorize`
- `cpt-cf-usage-collector-algo-usage-query-attribution-and-pdp-authorization-on-read`

**Constraints**: `SecurityContext`

**Touches**:

- Component: `cpt-cf-usage-collector-component-query-gateway`
- Entities: `SecurityContext`

### Entity: ResourceRef

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-entity-resource-ref`

The system **MUST** consume `ResourceRef` per DESIGN §3.1 — caller-supplied resource attribution (`resource_id` / `resource_type`) honored exclusively as a query-filter dimension intersected with the PDP-authorized scope under `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2` — never as a basis for widening the PDP-authorized scope; any user-supplied `ResourceRef` outside the `PdpConstraint` bound is silently clamped back to the constraint bound.

**Implements**:

- `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2`

**Constraints**: `ResourceRef`

**Touches**:

- Entities: `ResourceRef`

### API: POST /usage-collector/v1/records/aggregate

- [x] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-api-post-records-aggregate`

The system **MUST** expose `POST /usage-collector/v1/records/aggregate` per `usage-collector-v1.yaml` and DESIGN §3.3 — with the REST handler receiving `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`) and delegating to `UsageCollectorClientV1::query_aggregated_usage_records(ctx, ...)` per `sdk-trait.md` Method 3 — accept an `AggregationRequest` (`AggregationQuery`) carrying a typed `gts_id` (malformed → `InvalidArgument`, `.reason="INVALID_BASE_GTS_ID"`) and a closed-enum `aggregation` operator (absent / unsupported → `InvalidArgument`, HTTP `400`, at body deserialization), enforce the mandatory bounded time window via `require_bounded_time_window` (missing → `InvalidArgument`, `.reason="MISSING_TIME_WINDOW"`), perform per-component PDP authorization via the `access_scope_with` helper inside `cpt-cf-usage-collector-component-query-gateway` realizing `cpt-cf-usage-collector-flow-foundation-pdp-authorize` against `cpt-cf-usage-collector-contract-authz-resolver`, resolve the usage type pre-dispatch via a `get_usage_type` call (an unregistered `gts_id` → canonical `NotFound`, HTTP `404`, before dispatch) and enforce the op-per-kind restriction pre-dispatch (an op the resolved `kind` does not admit — `sum` on a gauge, or `min`/`max`/`avg` on a counter — → `InvalidArgument`, HTTP `400`, `field_violations[0].reason="OP_NOT_ALLOWED_FOR_KIND"`; counter admits `{sum, count}`, gauge admits `{min, max, avg, count}`), dispatch the composed filter set + time window + typed `gts_id` + aggregation operator + `group_by` keys to the Plugin SPI `query_aggregated_usage_records` capability via `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2` (the storage plugin stays pure-persistence and only ever receives an allowed `(op, kind)` pair), and return either a `AggregationResult` or one of the stable `rejected-validation` / `denied` / `unavailable` `Problem` envelopes (missing `SecurityContext` at the handler boundary surfaces the canonical `Unauthenticated` `Problem` envelope per the yaml's `default` response).

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-aggregated`
- `cpt-cf-usage-collector-algo-usage-query-plugin-spi-aggregate-dispatch-v2`

**Constraints**: `cpt-cf-usage-collector-interface-rest-api`

**Touches**:

- API: `POST /usage-collector/v1/records/aggregate`
- Component: `cpt-cf-usage-collector-component-usage-type-catalog`, `cpt-cf-usage-collector-component-query-gateway`

### API: GET /usage-collector/v1/records

- [ ] `p1` - **ID**: `cpt-cf-usage-collector-dod-usage-query-api-post-records-query`

The system **MUST** expose `GET /usage-collector/v1/records` per `usage-collector-v1.yaml` and DESIGN §3.3 with the OData query parameters `$filter`, `$orderby`, `limit`, `cursor` (`RawQuery`) — with the REST handler receiving `Extension<SecurityContext>` populated by ToolKit gateway middleware (`OperationBuilder::authenticated()`) and delegating to `UsageCollectorClientV1::list_usage_records(ctx, ...)` per `sdk-trait.md` Method 4; parse and validate the OData expressions, enforce the semantic mandatoriness of `timestamp ge X and timestamp lt Y` inside `$filter` at the gateway after OData parsing and before plugin dispatch (missing window → canonical `InvalidArgument` `Problem`, `field_violations[0].reason="MISSING_TIME_WINDOW"` on `$filter`), normalize the optional `$orderby` to end in the canonical unique `(created_at, id)` suffix in the caller order's direction (an omitted `$orderby` becoming `created_at asc, id asc`) and reject a caller order that mixes `asc` and `desc` or names an optional (nullable) field with `field_violations[0].reason="VALIDATION"` on `$orderby`, bound `limit` to the 1,000-record page cap (absent → the cap; a present value over the cap rejected gateway-side with `field_violations[0].reason="VALIDATION"`, never clamped), decode the optional `cursor` query parameter as a toolkit `CursorV1` value and validate it via `toolkit_odata::validate_cursor_against` against the parsed `$filter` AST and `$orderby` projection (`Malformed` → `INVALID_CURSOR`; `OrderMismatch` → `ORDER_MISMATCH`; `FilterMismatch` → `FILTER_MISMATCH`, each a `field_violations[0]` on `cursor`), perform per-component PDP authorization via the `access_scope_with` helper inside `cpt-cf-usage-collector-component-query-gateway` realizing `cpt-cf-usage-collector-flow-foundation-pdp-authorize` against `cpt-cf-usage-collector-contract-authz-resolver`, dispatch the structured tuple `(filter_ast: FilterNode<UsageRecordFilterField>, order_keys: OrderKeys, page_after: Option<Keyset>, limit: u32)` to the Plugin SPI `list_usage_records` capability via `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2` (the cursor wire format is NEVER forwarded to the plugin SPI), mint the next `CursorV1` from the plugin-returned `last_keyset` bound to the current `$filter` / `$orderby`, and return either a `toolkit_odata::Page<UsageRecord>` envelope (with an optional `@nextLink` containing the freshly minted gateway-owned `CursorV1`) or one of the stable `rejected-validation` / `denied` / `unavailable` canonical `toolkit_canonical_errors::Problem` envelopes (missing `SecurityContext` at the handler boundary surfaces the canonical `Unauthenticated` `Problem` envelope per the yaml's `default` response).

**Implements**:

- `cpt-cf-usage-collector-flow-usage-query-query-raw`
- `cpt-cf-usage-collector-algo-usage-query-cursor-pagination-orchestration-v2`
- `cpt-cf-usage-collector-algo-usage-query-plugin-spi-raw-page-dispatch-v2`

**Constraints**: `cpt-cf-usage-collector-interface-rest-api`

**Touches**:

- API: `GET /usage-collector/v1/records`
- Component: `cpt-cf-usage-collector-component-query-gateway`

### §2.4-item → DoD-ID Coverage Matrix

Coverage of every DECOMPOSITION §2.4 catalog item:

| §2.4 Item                                                                                                            | Kind              | DoD ID                                                                       |
| -------------------------------------------------------------------------------------------------------------------- | ----------------- | ---------------------------------------------------------------------------- |
| `cpt-cf-usage-collector-fr-query-aggregation`                                                                        | FR                | `cpt-cf-usage-collector-dod-usage-query-fr-query-aggregation`                |
| `cpt-cf-usage-collector-fr-query-raw`                                                                                | FR                | `cpt-cf-usage-collector-dod-usage-query-fr-query-raw`                        |
| `cpt-cf-usage-collector-fr-tenant-isolation`                                                                         | FR                | `cpt-cf-usage-collector-dod-usage-query-fr-tenant-isolation`                 |
| `cpt-cf-usage-collector-nfr-query-latency`                                                                           | NFR               | `cpt-cf-usage-collector-dod-usage-query-nfr-query-latency`                   |
| `cpt-cf-usage-collector-nfr-workload-isolation`                                                                      | NFR               | `cpt-cf-usage-collector-dod-usage-query-nfr-workload-isolation`              |
| `cpt-cf-usage-collector-principle-pdp-centric-authorization`                                                         | Principle         | `cpt-cf-usage-collector-dod-usage-query-principle-pdp-centric-authorization` |
| `cpt-cf-usage-collector-principle-fail-closed`                                                                       | Principle         | `cpt-cf-usage-collector-dod-usage-query-principle-fail-closed`               |
| `cpt-cf-usage-collector-constraint-no-business-logic`                                                                | Design constraint | `cpt-cf-usage-collector-dod-usage-query-constraint-no-business-logic`        |
| `cpt-cf-usage-collector-constraint-nfr-thresholds`                                                                   | Design constraint | `cpt-cf-usage-collector-dod-usage-query-constraint-nfr-thresholds`           |
| `cpt-cf-usage-collector-component-query-gateway`                                                                     | Design component  | `cpt-cf-usage-collector-dod-usage-query-component-query-gateway`             |
| `cpt-cf-usage-collector-seq-query-aggregated`                                                                        | Sequence          | `cpt-cf-usage-collector-dod-usage-query-seq-query-aggregated`                |
| `cpt-cf-usage-collector-seq-query-raw`                                                                               | Sequence          | `cpt-cf-usage-collector-dod-usage-query-seq-query-raw`                       |
| `cpt-cf-usage-collector-contract-downstream-usage-reader`                                                            | Contract          | `cpt-cf-usage-collector-dod-usage-query-contract-downstream-usage-reader`    |
| `AggregationQuery`                                                                    | Entity            | `cpt-cf-usage-collector-dod-usage-query-entity-aggregation-query`            |
| `AggregationResult`                                                                   | Entity            | `cpt-cf-usage-collector-dod-usage-query-entity-aggregation-result`           |
| `RawQuery`                                                                            | Entity            | `cpt-cf-usage-collector-dod-usage-query-entity-raw-query`                    |
| `cpt-cf-usage-collector-principle-cursor-gateway-ownership`                                                          | Policy            | `cpt-cf-usage-collector-dod-usage-query-cursor-v1-toolkit-adoption`           |
| `PdpConstraint`                                                                       | Entity            | `cpt-cf-usage-collector-dod-usage-query-entity-pdp-constraint`               |
| `SecurityContext`                                                                     | Entity            | `cpt-cf-usage-collector-dod-usage-query-entity-security-context`             |
| `ResourceRef`                                                                         | Entity            | `cpt-cf-usage-collector-dod-usage-query-entity-resource-ref`                 |
| `POST /usage-collector/v1/records/aggregate`                                                                         | API               | `cpt-cf-usage-collector-dod-usage-query-api-post-records-aggregate`          |
| `GET /usage-collector/v1/records`                                                                                    | API               | `cpt-cf-usage-collector-dod-usage-query-api-post-records-query`              |

## 6. Acceptance Criteria

### 6.1 Endpoints Summary

The feature's REST surface is aligned with the phase-03 OAS reference contract (`usage-collector-v1.yaml`) and the phase-04 DESIGN.md §3.3 Endpoints Overview table. The runtime OAS is emitted at runtime by `OpenApiRegistryImpl` from `OperationBuilder` calls; the YAML is the documentary reference enforced by the CI drift-check.

| Operation                   | Method | Path                                    | OperationId                                | Tag   |
| --------------------------- | ------ | --------------------------------------- | ------------------------------------------ | ----- |
| Raw read (cursor-paginated) | `GET`  | `/usage-collector/v1/records`           | `usage_collector.query_raw_records`        | Query |
| Aggregated read (body)      | `POST` | `/usage-collector/v1/records/aggregate` | `usage_collector.query_aggregated_records` | Query |

Query parameters for the raw read: `$filter` (OData predicate over `UsageRecordFilterField`; mandatory `timestamp ge X and timestamp lt Y` window), `$orderby` (optional; normalized gateway-side to end in the canonical unique `(created_at, id)` suffix — mixed directions or an optional/nullable key → `400`, `.reason="VALIDATION"`), `limit` (absent → 1000; a present value > 1000 → `400 InvalidArgument`, `field_violations[0].reason="VALIDATION"`, not clamped), `cursor` (toolkit `CursorV1` opaque token decoded and validated at the gateway via `toolkit_odata::validate_cursor_against`).

Response envelope for the raw read: `toolkit_odata::Page<UsageRecord>` (`items`, optional `@nextLink`). Response for the aggregated read: `AggregationResult` (typed body; no `@nextLink`, no pagination — see aggregate-asymmetry rationale at `cpt-cf-usage-collector-principle-aggregate-asymmetry`).

### 6.2 Behavioural Criteria

- [ ] `p1` - A well-formed aggregated read by an authorized caller through `POST /usage-collector/v1/records/aggregate` (or the SDK `query_aggregated_usage_records` operation per `sdk-trait.md`) carrying a structurally valid `[from, to)` `time_range`, exactly one `gts_id` filter that resolves via a per-query `get_usage_type` SPI dispatch against `cpt-cf-usage-collector-contract-storage-plugin`, and a mandatory `aggregation` operator drawn from `{SUM, COUNT, MIN, MAX, AVG}` produces a `AggregationResult` (`gts_id`, `aggregation`, `buckets`) computed server-side by the Plugin SPI `query_aggregated_usage_records` capability over the persisted usage records; aggregated queries that omit the bounded time window (→ `InvalidArgument`, `.reason="MISSING_TIME_WINDOW"`) or omit / supply an unsupported `aggregation` operator (→ `InvalidArgument`, HTTP `400`, at body deserialization) are rejected before any Plugin SPI aggregate dispatch (aggregated success and pre-dispatch validation).
- [ ] `p1` - A well-formed raw read by an authorized caller through `GET /usage-collector/v1/records` (or the SDK `list_usage_records` operation per `sdk-trait.md`) carrying a structurally valid `$filter` over `UsageRecordFilterField` that includes the mandatory `timestamp ge X and timestamp lt Y` window, optional narrowing predicates (`tenant_id` / `subject_id` / `subject_type` / `resource_id` / `resource_type` / `status` per `usage-collector-v1.yaml`), an optional `$orderby` the gateway normalizes to end in the canonical unique `(created_at, id)` suffix, a `limit` within the 1,000-records-per-page cap, and an optional toolkit `CursorV1` continuation token in the `cursor` query parameter returns a `toolkit_odata::Page<UsageRecord>` envelope (`items`, optional `@nextLink` containing a freshly minted gateway-owned `CursorV1`) deterministically resumable across calls by forwarding the prior response's `@nextLink` cursor token back into the next request; a malformed cursor surfaces as a canonical `InvalidArgument` `Problem` with `field_violations[0].reason="INVALID_CURSOR"`, a cursor minted against a different `$orderby` with `"ORDER_MISMATCH"`, a cursor minted against a different `$filter` with `"FILTER_MISMATCH"` (each on the `cursor` field), and a missing mandatory `timestamp ge X and timestamp lt Y` window with `"MISSING_TIME_WINDOW"` on `$filter` (cursor decode + validate at the gateway via `toolkit_odata::validate_cursor_against`; the plugin SPI never receives the cursor wire format) and the request leaks no records (raw pagination success and cursor-V1 adoption, `cpt-cf-usage-collector-dod-usage-query-cursor-v1-toolkit-adoption`, and `cpt-cf-usage-collector-dod-usage-query-api-post-records-query`).
- [ ] `p1` - Every aggregated and raw read composes the foundation-returned `PdpConstraint` set with the caller's request filters under intersection-only semantics via `cpt-cf-usage-collector-algo-usage-query-pdp-constraint-composition-v2`; any caller-supplied filter that attempts to widen scope beyond a constraint bound (e.g., a tenant outside the PDP-permitted tenants or a UsageType outside a PDP-permitted UsageType set) is silently clamped back to the constraint bound and the effective query never broadens the PDP-authorized scope under any input — verifiable by exercising a widening attempt and observing that the returned row set is bounded by the PDP constraint, not the caller's filter (PDP narrowing).
- [ ] `p1` - An aggregated query whose `gts_id` is absent from the plugin's `usage_type_catalog` is rejected as canonical `NotFound` (404) — a pre-dispatch `get_usage_type` (Method 7) call surfaces `Err(UsageTypeNotFound { gts_id })` before any aggregate dispatch, lifted to `UsageCollectorError::NotFound` (`resource_type="usage_type"`, `resource_name=<gts_id>`); the gateway MUST NOT return a partial result and MUST NOT reach the `query_aggregated_usage_records` dispatch (UsageType existence enforcement, `cpt-cf-usage-collector-dod-usage-query-entity-aggregation-query`, and `cpt-cf-usage-collector-fr-usage-type-existence-and-semantics`).
- [ ] `p1` - When the inbound `SecurityContext` is missing at the handler boundary (REST handler did not receive `Extension<SecurityContext>` from ToolKit gateway middleware, or the SDK trait was invoked without a `ctx` argument) the gateway returns the canonical `Unauthenticated` `Problem` envelope; when `cpt-cf-usage-collector-flow-foundation-pdp-authorize` (invoked via the per-component `access_scope_with` helper inside `cpt-cf-usage-collector-component-query-gateway` against `cpt-cf-usage-collector-contract-authz-resolver`) returns `deny` / yields an empty `PdpConstraint` set / is unreachable, or the Plugin SPI `query_aggregated_usage_records` / `list_usage_records` capability returns host-resolution `PluginUnavailable` / plugin-side `Transient` / `Internal`, the gateway returns the corresponding fail-closed `Problem` envelope per `usage-collector-v1.yaml`; in every case the gateway never synthesizes a partial aggregation or partial page, never caches a prior PDP decision, never synthesizes or infers identity, and surfaces zero records — verifiable by injecting each failure mode independently and observing the corresponding `Problem` envelope (fail-closed posture).
- [ ] `p1` - Every aggregated and raw read derives `tenant_id` exclusively from the foundation-resolved `SecurityContext` and the PDP-returned `PdpConstraint` set; cross-tenant reads are impossible absent an explicit platform PDP permit, no caller-supplied `tenant_id` filter or header escapes the `SecurityContext` binding (any widening attempt is silently clamped), and a tenant-administrator caller observes only rows scoped to their own tenant — verifiable by issuing a query with a caller-supplied `tenant_id` outside the resolved `SecurityContext` and confirming the returned scope is clamped back to the PDP-permitted tenants (tenant isolation).
- [ ] `p1` - Within the PDP-authorized scope, both `active` and `inactive` `UsageRecord` rows are visible to query callers — both states contribute to the aggregated `buckets` and each raw record surfaces its `status` field verbatim — and the Query Gateway never filters rows by activation state, never performs the `active → inactive` flip (deactivation is owned by §2.5 Event Deactivation), and never overrides the `status` value returned by the storage plugin (active-and-inactive visibility).
- [ ] `p1` - An authorized aggregated or raw query whose filters match zero rows within the PDP-authorized scope returns an empty `AggregationResult` (`buckets` is the empty list) or an empty `toolkit_odata::Page<UsageRecord>` envelope (`items` is the empty list and `@nextLink` is omitted); zero matches MUST NOT surface as an HTTP `404`, an error envelope, a Plugin SPI error, or any non-200 outcome — verifiable by issuing a filter that is known to match nothing and confirming a `200 OK` with an empty payload (empty-match semantics, `cpt-cf-usage-collector-dod-usage-query-fr-query-raw`, `cpt-cf-usage-collector-dod-usage-query-entity-aggregation-result`, and `cpt-cf-usage-collector-dod-usage-query-cursor-v1-toolkit-adoption`).
- [ ] `p1` - Every accepted aggregated and raw read honours the downstream usage-reader contract surface served by `cpt-cf-usage-collector-component-query-gateway` per DESIGN §3.5 Downstream Usage Reader Contract — the documented request shapes (`AggregationQuery`, `RawQuery`), the documented response shapes (`AggregationResult`, `toolkit_odata::Page<UsageRecord>`, toolkit `CursorV1`), the stable error categories (`InvalidArgument` with `field_violations[0].reason` ∈ {`MISSING_TIME_WINDOW`, `INVALID_CURSOR`, `ORDER_MISMATCH`, `FILTER_MISMATCH`, `INVALID_ORDERBY_FIELD`, and `VALIDATION` for a `limit` over the page cap}; `PermissionDenied`; `NotFound` for an unregistered usage type; `ServiceUnavailable` per `usage-collector-v1.yaml`), the gateway-owned cursor decode + validate guarantee, the PDP-narrowed scope semantics, and the active-and-inactive record visibility rule — and surfaces values verbatim from the storage plugin without business-logic transformation (no pricing, rating, invoice generation, quota enforcement, unit conversion, currency conversion, or rule-based filtering); any deviation surfaces as a contract-test failure against `usage-collector-v1.yaml` (downstream contract).
- [ ] `p1` - `SUM` over a `(tenant_id, gts_id)` group that contains both a row with `corrects_id IS NULL` and a row with `corrects_id IS NOT NULL` MUST equal the **signed net total** — `SUM(value)` aggregates across active rows regardless of `corrects_id` presence, treating `value` as a signed quantity so rows with `corrects_id IS NOT NULL` (carrying a strictly-negative `value`) reduce the running counter total; verifiable by emitting a usage row with `value = +10` and `corrects_id IS NULL`, a compensation row with `value = -3` and `corrects_id` pointing at the usage row, and observing `SUM(value) = +7` on the aggregated read. The same construction with a single usage row and no compensations MUST yield `SUM(value) = +10` (unchanged); compensation rows whose referenced usage row has been deactivated (and which therefore cascaded to `inactive` per the depth-1 cascade owned by `cpt-cf-usage-collector-feature-event-deactivation`) MUST NOT contribute to `SUM` (`SUM` returns to `0` after the cascade) — the `active`-status filter is applied before the `corrects_id`-aware aggregation (SUM-nets contract).
- [ ] `p1` - `COUNT` over the same `(tenant_id, gts_id)` group that contains a row with `corrects_id IS NULL` and a row with `corrects_id IS NOT NULL` MUST equal **1** — counting rows with `corrects_id IS NOT NULL` as events would double-count the original usage event because the row referenced by the compensation's `corrects_id` is already counted; `MIN(value)`, `MAX(value)`, and `AVG(value)` over the same group MUST be computed over active rows WHERE `corrects_id IS NULL` — including the strictly-negative compensation `value` would corrupt extremes (the refund would become the new `MIN`) and means (the mean would drift below the observed usage range). Verifiable by adding a compensation row with `value = -3` and `corrects_id` set to a group with a single usage row of `value = +10` and `corrects_id IS NULL`, and confirming `COUNT = 1`, `MIN = +10`, `MAX = +10`, `AVG = +10` (compensation rows excluded from all four aggregates) (usage-only aggregation).
- [ ] `p1` - The aggregation contract is orthogonal to status filtering: deactivated rows (whether the row was directly deactivated with `corrects_id IS NULL`, deactivated with `corrects_id IS NOT NULL`, or flipped to `inactive` via the depth-1 cascade owned by `cpt-cf-usage-collector-feature-event-deactivation`) MUST be excluded from all five aggregations (`SUM` / `COUNT` / `MIN` / `MAX` / `AVG`) before netting / counting / extremes / means are computed; verifiable by deactivating either the usage row or one of its referencing compensation rows and confirming that the post-cascade `SUM` returns to a state consistent with the remaining `active` rows in the group while `COUNT` / `MIN` / `MAX` / `AVG` likewise reflect only the remaining `active` rows WHERE `corrects_id IS NULL` (orthogonality of `active` filtering and `corrects_id`-presence filtering).
- [ ] `p1` - Aggregation ops are restricted per usage `kind`: `SUM` against a **gauge** usage type, and `MIN` / `MAX` / `AVG` against a **counter** usage type, are each rejected with `InvalidArgument` (HTTP `400`, `field_violations[0].reason="OP_NOT_ALLOWED_FOR_KIND"`) **before** any Plugin SPI aggregate dispatch; `COUNT` is accepted on both kinds, `SUM` on a counter and `MIN` / `MAX` / `AVG` on a gauge are dispatched. The gateway resolves the `kind` via a per-query `get_usage_type` SPI dispatch (which also makes an unregistered `gts_id` a pre-dispatch `NotFound`), and the storage plugin stays pure-persistence (op-per-kind restriction).
- [ ] `p2` - Every query attempt on either read path is observable through the four query-gateway instruments inventoried in DESIGN [§3.11.5](../DESIGN.md#3115-operational-metric-inventory-ops-design-002): a completed attempt increments `uc_query_requests_total` exactly once with the correct `(query_kind, outcome, error_category)` tuple (`error_category="none"` only when `outcome="success"`) and observes `uc_query_duration_seconds{query_kind}` exactly once; the `uc_query_inflight{query_kind}` gauge rises by one once authorization composes and returns to its prior value on completion and on every failure exit that follows the increment (no gauge leak under an early return); a successful completion additionally records `uc_query_result_rows{query_kind}` exactly once with the raw page size (`items` length, ≤ 1,000) or the aggregated group count (`buckets` length, ≤ 100,000); and no label on any of the four instruments carries an unbounded identifier — verifiable by issuing, per `query_kind`, one successful, one PDP-denied, and one plugin-failure request against a test meter and asserting the exact counter deltas, histogram sample counts, gauge round-trip, and label values against the closed DESIGN §3.11.5 vocabularies (query-path telemetry, `cpt-cf-usage-collector-dod-usage-query-nfr-operational-visibility`).
