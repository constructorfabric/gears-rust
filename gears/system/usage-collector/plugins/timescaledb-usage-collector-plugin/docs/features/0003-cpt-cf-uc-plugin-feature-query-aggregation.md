# Feature: Query & Aggregation

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Out of Scope](#15-out-of-scope)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Aggregated Query (Pushed-Down)](#aggregated-query-pushed-down)
  - [Keyset-Paginated Raw List](#keyset-paginated-raw-list)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Injection-Safe Query Translation](#injection-safe-query-translation)
  - [Keyset Look-Ahead & Cursor Encoding](#keyset-look-ahead--cursor-encoding)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement pushed-down aggregation](#implement-pushed-down-aggregation)
  - [Implement keyset-paginated raw list](#implement-keyset-paginated-raw-list)
  - [Implement injection-safe translation](#implement-injection-safe-translation)
  - [Reject a nullable order key](#reject-a-nullable-order-key)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Applicable Concerns](#7-non-applicable-concerns)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-featstatus-query-aggregation-implemented`

<!-- reference to DECOMPOSITION entry -->

- [ ] `p1` - `cpt-cf-uc-plugin-feature-query-aggregation`

## 1. Feature Context

### 1.1 Overview

The backend read plane: pushed-down SUM/COUNT/MIN/MAX/AVG aggregation with grouping, and a keyset-paginated raw list over the canonical `(created_at, id)` order, both driven by injection-safe query translation.

### 1.2 Purpose

Pushes analytical work into TimescaleDB so its native acceleration does it — aggregation executes inside the backend over the active-row set with the host filter and scope applied, never returning raw rows for client-side aggregation. The raw list uses keyset (seek) pagination with a one-row look-ahead and next-cursor encoding, refusing an order key on a nullable field so no matching record is silently dropped. All translation binds comparison values as parameters and resolves identifiers through a closed allowlist. This feature is the allocation target for the aggregation query-latency NFR.

**Requirements**: `cpt-cf-uc-plugin-fr-aggregated-query`, `cpt-cf-uc-plugin-fr-raw-query`, `cpt-cf-uc-plugin-nfr-query-latency`

**Principles**: `cpt-cf-uc-plugin-principle-pure-persistence` (realized; owned by Feature 1)

**Constraints**: `cpt-cf-uc-plugin-constraint-injection-safe-translation`

### 1.3 Actors

| Actor                                | Role in Feature                                                                                                                          |
| ------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `cpt-cf-uc-plugin-actor-plugin-host` | Supplies the PDP-scoped `ODataQuery` filter, the `AggregationSpec`, and the optional cursor; consumes the `AggregationResult` or `Page`. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Decomposition**: `cpt-cf-uc-plugin-feature-query-aggregation`
- **Design elements**: `cpt-cf-uc-plugin-component-record-store` (reader; owned by Feature 2), `cpt-cf-uc-plugin-dbtable-usage-records` (reader)
- **Sequences**: `cpt-cf-uc-plugin-seq-query-aggregated`, `cpt-cf-uc-plugin-seq-list-keyset`
- **Dependencies**: `cpt-cf-uc-plugin-feature-foundation`, `cpt-cf-uc-plugin-feature-record-persistence`

### 1.5 Out of Scope

- Writing, dedup, compensation, or deactivation of records — Feature 2 (`cpt-cf-uc-plugin-feature-record-persistence`).
- Catalog listing keyset pagination (`list_usage_types`) — reuses this same keyset pattern but is owned by Feature 4 (`cpt-cf-uc-plugin-feature-usage-type-catalog`) against the Catalog Store.
- The `usage_records` table DDL and row-writer ownership — created by Feature 1, written by Feature 2; this feature is a reader and re-owns neither.

## 2. Actor Flows (CDSL)

### Aggregated Query (Pushed-Down)

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-flow-query-aggregated`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Success Scenarios**:

- An `AggregationResult` is returned, computed in the backend over the active-row set honoring the host filter and scope.

**Error Scenarios**:

- An unrecognized (non-allowlisted) filter or group-by identifier is rejected as `Internal` rather than emitted into SQL.

**Steps**:

1. [ ] - `p1` - Host calls `query_aggregated_usage_records(spec, ODataQuery)` - `inst-agg-1`
2. [ ] - `p1` - Translate the filter and `AggregationSpec` to SQL via `cpt-cf-uc-plugin-algo-query-translation` (scope predicate, aggregate operation, GROUP BY dimensions) - `inst-agg-2`
3. [ ] - `p1` - DB: SELECT the aggregate of `value` over the active-row set, GROUP BY the requested dimensions - `inst-agg-3`
4. [ ] - `p1` - Map the returned bucket rows into an `AggregationResult` - `inst-agg-4`
5. [ ] - `p1` - **RETURN** the `AggregationResult` (never raw rows for client-side aggregation) - `inst-agg-5`

### Keyset-Paginated Raw List

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-flow-query-list-keyset`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Success Scenarios**:

- A `Page` of `UsageRecord` is returned in `(created_at, id)` order with a next cursor when a further page exists.

**Error Scenarios**:

- An `$orderby` on a domain-optional (nullable) field is rejected fail-closed so no matching record is silently dropped.

**Steps**:

1. [ ] - `p1` - Host calls `list_usage_records(ODataQuery, optional cursor)` - `inst-lst-1`
2. [ ] - `p1` - Decode `CursorV1` into the `(created_at, id)` seek key via `cpt-cf-uc-plugin-algo-query-keyset-encode` - `inst-lst-2`
3. [ ] - `p1` - Re-check the requested order key fail-closed; reject a nullable ordering key - `inst-lst-3`
4. [ ] - `p1` - DB: SELECT the page WHERE filter AND rows after the seek key, ORDER BY (created_at, id), LIMIT effective_page_size + 1 - `inst-lst-4`
5. [ ] - `p1` - Trim to the page and encode the next `CursorV1` from the last in-page row - `inst-lst-5`
6. [ ] - `p1` - **RETURN** the `Page` of `UsageRecord` with the optional next cursor - `inst-lst-6`

## 3. Processes / Business Logic (CDSL)

### Injection-Safe Query Translation

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-algo-query-translation`

**Input**: The gateway-supplied `ODataQuery` (scope/tenant predicate, time bounds, metadata predicates) and, for aggregation, the `AggregationSpec`.

**Output**: Parameterized SQL where no caller-supplied string reaches the query text.

**Steps**:

1. [ ] - `p1` - Bind every comparison value (scope/tenant predicate, time bounds, cursor seek key) as a `sqlx` parameter, never concatenated - `inst-xlate-1`
2. [ ] - `p1` - **FOR EACH** identifier that must appear as a SQL column (top-level filter/order column, GROUP BY column) - `inst-xlate-2`
   1. [ ] - `p1` - Resolve it through the closed allowlist of `usage_records` columns - `inst-xlate-2a`
   2. [ ] - `p1` - **IF** the identifier is unrecognized - `inst-xlate-2b`
      1. [ ] - `p1` - **RETURN** `Internal` (reject rather than emit) - `inst-xlate-2b1`
3. [ ] - `p1` - **IF** the predicate is over `metadata` - `inst-xlate-3`
   1. [ ] - `p1` - Compile to `metadata ->> $key <op> $value` with both the JSONB key and the compared value bound as parameters (no key enumeration, no allowlist) - `inst-xlate-3a`
4. [ ] - `p1` - **RETURN** the parameterized SQL - `inst-xlate-4`

### Keyset Look-Ahead & Cursor Encoding

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-algo-query-keyset-encode`

**Input**: The host order, the optional decoded `CursorV1`, and the requested page size.

**Output**: The seek predicate, the effective limit, and the next `CursorV1`.

**Steps**:

1. [ ] - `p1` - Floor the effective page size to 1 so a `$top=0` that slips the gateway cannot underflow the `LIMIT n+1` look-ahead - `inst-cur-1`
2. [ ] - `p1` - Build the seek predicate as a row-value tuple comparison over the `NOT NULL` `(created_at, id)` order - `inst-cur-2`
3. [ ] - `p1` - Re-check the order key fail-closed via the SDK keyset-safety guard; reject a nullable ordering field (`subject_id` / `subject_type` / `corrects_id`) - `inst-cur-3`
4. [ ] - `p1` - Fetch `effective_page_size + 1` rows to detect a next page, then trim to the page - `inst-cur-4`
5. [ ] - `p1` - Encode the next `CursorV1` from the last in-page row - `inst-cur-5`
6. [ ] - `p1` - **RETURN** the seek predicate, effective limit, and next cursor - `inst-cur-6`

## 4. States (CDSL)

Not applicable — the read plane is stateless; it holds no entity lifecycle state. Pagination position is carried by the opaque `CursorV1`, not by server-side state.

## 5. Definitions of Done

### Implement pushed-down aggregation

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-dod-query-aggregation`

The system **MUST** execute SUM/COUNT/MIN/MAX/AVG with grouping inside the backend over the active-row set, applying the host filter and scope, and **MUST NOT** return raw rows for client-side aggregation.

**Implements**:

- `cpt-cf-uc-plugin-flow-query-aggregated`

**Constraints**: `cpt-cf-uc-plugin-nfr-query-latency`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-record-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`
- Entities: `AggregationSpec`, `AggregationResult`, `ODataQuery`

### Implement keyset-paginated raw list

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-query-keyset-list`

The system **MUST** return raw records as keyset-paginated pages over `(created_at, id)` honoring the supplied order and cursor, with a one-row look-ahead, next-cursor encoding, and an effective page size floored to 1. It **MUST NOT** widen the host-supplied filter.

**Implements**:

- `cpt-cf-uc-plugin-flow-query-list-keyset`
- `cpt-cf-uc-plugin-algo-query-keyset-encode`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-record-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`
- Entities: `ODataQuery`, `CursorV1`, `Page`, `UsageRecord`

### Implement injection-safe translation

- [ ] `p1` - **ID**: `cpt-cf-uc-plugin-dod-query-injection-safe`

The system **MUST** build all filter, aggregation, and pagination SQL with comparison values passed as bound parameters and caller-influenced identifiers resolved through a closed `usage_records` column allowlist (unrecognized → `Internal`); metadata predicates **MUST** bind both the JSONB key and the compared value as parameters.

**Implements**:

- `cpt-cf-uc-plugin-algo-query-translation`

**Constraints**: `cpt-cf-uc-plugin-constraint-injection-safe-translation`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-record-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`

### Reject a nullable order key

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-query-nullable-orderby`

The system **MUST** reject fail-closed an `$orderby` on a domain-optional (nullable) field so a crafted cursor cannot smuggle a nullable ordering key past the SQL boundary and silently drop rows.

**Implements**:

- `cpt-cf-uc-plugin-algo-query-keyset-encode`

**Constraints**: `cpt-cf-uc-plugin-constraint-injection-safe-translation`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-record-store`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`

## 6. Acceptance Criteria

- [ ] Aggregation (SUM/COUNT/MIN/MAX/AVG) with grouping is computed in the backend over the active-row set and honors the host filter and scope; no raw rows are returned for client-side aggregation.
- [ ] Raw list returns keyset-paginated pages over `(created_at, id)` honoring the supplied order and cursor, with a next cursor when a further page exists.
- [ ] No caller-supplied string reaches query text as a literal or identifier: values are bound parameters, identifiers are allowlisted, and an unrecognized identifier is rejected as `Internal`.
- [ ] A metadata predicate binds both the JSONB key and the compared value as parameters without enumerating keys.
- [ ] An `$orderby` on a nullable field is rejected fail-closed; a `$top=0` cannot underflow the look-ahead (effective page size floored to 1).
- [ ] Aggregation over a 30-day single-tenant range completes within 500ms at p95 within the parent throughput-profile envelope.

## 7. Non-Applicable Concerns

- **Security — Authentication & Authorization (SEC-FDESIGN-001, SEC-FDESIGN-002)**: Not applicable — the query arrives already PDP-scoped; the plugin never widens the host filter and performs no authorization. Its query-security obligation is injection safety (`cpt-cf-uc-plugin-dod-query-injection-safe`), which is addressed.
- **Data Integrity — Write/Transaction (REL-FDESIGN-003)**: Not applicable — the read plane performs no writes and opens no write transaction; write integrity is Feature 2.
- **Data Retention (DATA-FDESIGN-004)**: Not applicable here — retention is Feature 5; queries read whatever rows currently exist within the retention window.
- **Usability (UX) / Compliance (COMPL)**: Not applicable — no user interface and no plugin-level regulatory obligation; inherited from Feature 1's dispositions.
- **Observability (OPS-FDESIGN-001)**: Instrumented cross-cuttingly by Feature 6 (query-duration histogram and query-requests counter labeled by `query_kind`).
- **Caching (INT-FDESIGN-005)**: Not applicable — the plugin holds no query cache; freshness follows the published consistency profile (Feature 1) and TimescaleDB executes each query directly.
