# Feature: Backend Observability & Metrics

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Out of Scope](#15-out-of-scope)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Instrumented SPI Dispatch](#instrumented-spi-dispatch)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Backend-Readiness Gauge](#backend-readiness-gauge)
  - [Bounded Label Application](#bounded-label-application)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement the metric inventory](#implement-the-metric-inventory)
  - [Implement the backend-readiness gauge](#implement-the-backend-readiness-gauge)
  - [Record SQL work under the host span](#record-sql-work-under-the-host-span)
  - [Enforce bounded label cardinality](#enforce-bounded-label-cardinality)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Applicable Concerns](#7-non-applicable-concerns)

<!-- /toc -->

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-featstatus-observability-implemented`

<!-- reference to DECOMPOSITION entry -->

- [ ] `p2` - `cpt-cf-uc-plugin-feature-observability`

## 1. Feature Context

### 1.1 Overview

The `uc_timescaledb_*` OpenTelemetry metric inventory that instruments the other five features' backend-internal operation, plus the plugin-local backend-health gauge and per-dispatch tracing-span attribution — a signal set distinct from the gear's request-path metrics.

### 1.2 Purpose

Emits the backend-internal telemetry the gear cannot see, under the plugin's own `uc_timescaledb_*` sub-namespace exported via OTLP. The inventory covers performance (insert / query / deactivate / pool-acquire duration histograms), efficiency (pool gauges, dedup-absorbed/stale counters, batch-row histogram), reliability (backend errors by classification, batch retries, idempotency conflicts, usage-type-referenced rejections, migration failures, and the `uc_timescaledb_ready` health gauge), security (TLS handshake failures), and catalog/workload (catalog size, compensations, query-request mix). Histogram bucket layouts bracket the NFR p95 budgets and are part of the contract; instrument names are full literal Prometheus names with no unit hint; unbounded identifiers are never used as labels. Each SPI dispatch records its SQL work under the host's ambient tracing span, so backend latency is attributable end-to-end through the host `trace_id`.

**Requirements**: `cpt-cf-uc-plugin-nfr-operational-visibility`

### 1.3 Actors

| Actor                                | Role in Feature                                                                                                     |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------------- |
| `cpt-cf-uc-plugin-actor-plugin-host` | Issues the SPI dispatches whose backend-internal work the plugin instruments under the host's ambient tracing span. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Decomposition**: `cpt-cf-uc-plugin-feature-observability`
- **Design elements**: `cpt-cf-uc-plugin-design-metric-inventory`
- **Dependencies**: `cpt-cf-uc-plugin-feature-foundation`

### 1.5 Out of Scope

- The request-path `usage_collector.*` signals and the host-computed structural `usage_collector.plugin.ready` gauge — owned by the gear core.
- The operations being measured (insert, query, catalog, deactivation, retention) — owned by Features 2–5; this feature instruments them cross-cuttingly and re-owns none.

## 2. Actor Flows (CDSL)

### Instrumented SPI Dispatch

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-flow-metrics-dispatch`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Success Scenarios**:

- Each SPI dispatch records its duration, outcome counters, and SQL work under the host's ambient tracing span; the emitted series carry only bounded labels.

**Error Scenarios**:

- On a pool-acquire failure the readiness gauge is cleared; it re-arms on the next successful acquire.

**Steps**:

1. [ ] - `p2` - Host issues an SPI call inside its ambient tracing span - `inst-md-1`
2. [ ] - `p2` - Record the SQL work under that span (the plugin opens no root span) - `inst-md-2`
3. [ ] - `p2` - Observe the operation's duration histogram (insert / query / deactivate / pool-acquire) and increment its outcome counters (dedup absorbed/stale, idempotency conflicts, usage-type referenced, compensations, query requests) - `inst-md-3`
4. [ ] - `p2` - Apply only bounded labels (`mode`, `query_kind`, `error_category`) via `cpt-cf-uc-plugin-algo-metrics-label-cardinality` - `inst-md-4`
5. [ ] - `p2` - Update the readiness gauge via `cpt-cf-uc-plugin-algo-metrics-readiness-gauge` - `inst-md-5`
6. [ ] - `p2` - **RETURN** with the signal emitted through the OTLP push exporter - `inst-md-6`

## 3. Processes / Business Logic (CDSL)

### Backend-Readiness Gauge

- [ ] `p3` - **ID**: `cpt-cf-uc-plugin-algo-metrics-readiness-gauge`

**Input**: Pool build / migration completion and pool-acquire outcomes.

**Output**: The plugin-local `uc_timescaledb_ready` gauge value (distinct from the host-computed structural readiness gauge).

**Steps**:

1. [ ] - `p2` - **IF** the pool build and migrations completed successfully - `inst-rg-1`
   1. [ ] - `p2` - Set `uc_timescaledb_ready` to 1 - `inst-rg-1a`
2. [ ] - `p2` - **IF** a pool acquire fails - `inst-rg-2`
   1. [ ] - `p2` - Clear `uc_timescaledb_ready` to 0 - `inst-rg-2a`
3. [ ] - `p2` - **IF** a subsequent acquire succeeds - `inst-rg-3`
   1. [ ] - `p2` - Re-arm `uc_timescaledb_ready` to 1 - `inst-rg-3a`
4. [ ] - `p2` - **RETURN** the gauge value (not a background probe) - `inst-rg-4`

### Bounded Label Application

- [ ] `p3` - **ID**: `cpt-cf-uc-plugin-algo-metrics-label-cardinality`

**Input**: A metric observation and its candidate labels.

**Output**: An emitted series carrying only enumerated, bounded labels.

**Steps**:

1. [ ] - `p2` - Attach only labels from the enumerated sets (`mode` ∈ {single, batch}; `query_kind` ∈ {aggregated, raw}; `error_category` ∈ {transient, internal}) - `inst-lc-1`
2. [ ] - `p2` - Reject unbounded identifiers (`tenant_id`, `gts_id`, `id`, `idempotency_key`, `corrects_id`, `request_id`, `trace_id`) as metric labels - `inst-lc-2`
3. [ ] - `p2` - Emit the instrument under its full literal Prometheus name (snake_case, `_total` on counters, `_seconds` on duration histograms) with no unit hint - `inst-lc-3`
4. [ ] - `p2` - **RETURN** the bounded-cardinality series - `inst-lc-4`

## 4. States (CDSL)

Not applicable — the metric instruments carry no persisted entity lifecycle. The `uc_timescaledb_ready` gauge is a set/clear/re-arm value driven by pool-acquire outcomes (§3), not an entity state machine.

## 5. Definitions of Done

### Implement the metric inventory

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-metrics-inventory`

The system **MUST** emit the `uc_timescaledb_*` performance, efficiency, reliability, security, and catalog/workload metric groups over OTLP, with histogram bucket layouts bracketing the NFR p95 budgets and full literal instrument names carrying no unit hint.

**Implements**:

- `cpt-cf-uc-plugin-flow-metrics-dispatch`

**Constraints**: `cpt-cf-uc-plugin-nfr-operational-visibility`

**Touches**:

- Design: `cpt-cf-uc-plugin-design-metric-inventory`

### Implement the backend-readiness gauge

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-metrics-readiness`

The system **MUST** maintain the plugin-local `uc_timescaledb_ready` gauge — set after a successful pool build + migration, cleared on a pool-acquire failure, re-armed on the next successful acquire — distinct from the host-computed structural readiness gauge and not a background probe.

**Implements**:

- `cpt-cf-uc-plugin-algo-metrics-readiness-gauge`

**Touches**:

- Design: `cpt-cf-uc-plugin-design-metric-inventory`

### Record SQL work under the host span

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-metrics-tracing-span`

The system **MUST** record each SPI dispatch's SQL work under the host's ambient tracing span, opening no root span, so backend latency is attributable end-to-end through the host `trace_id`.

**Implements**:

- `cpt-cf-uc-plugin-flow-metrics-dispatch`

**Touches**:

- Design: `cpt-cf-uc-plugin-design-metric-inventory`

### Enforce bounded label cardinality

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-metrics-label-cardinality`

The system **MUST** restrict metric labels to the enumerated bounded sets and **MUST NOT** use unbounded identifiers (`tenant_id`, `gts_id`, `id`, `idempotency_key`, `corrects_id`, `request_id`, `trace_id`) as labels.

**Implements**:

- `cpt-cf-uc-plugin-algo-metrics-label-cardinality`

**Constraints**: `cpt-cf-uc-plugin-nfr-operational-visibility`

**Touches**:

- Design: `cpt-cf-uc-plugin-design-metric-inventory`

## 6. Acceptance Criteria

- [ ] The plugin emits the enumerated `uc_timescaledb_*` metrics under its own sub-namespace over OTLP, distinct from the gear's request-path signals.
- [ ] A backend-readiness signal (`uc_timescaledb_ready`) is set after a successful pool build + migration, cleared on a pool-acquire failure, and re-armed on the next successful acquire.
- [ ] Instrument names are full literal Prometheus names (snake_case, `_total` / `_seconds`) with no unit hint, and histogram buckets bracket the NFR p95 budgets.
- [ ] Metric labels are bounded to the enumerated sets; no unbounded identifier is used as a label.
- [ ] Each SPI dispatch's SQL work is recorded under the host's ambient tracing span (no plugin root span), attributable through the host `trace_id`.

## 7. Non-Applicable Concerns

- **Security — Authentication, Authorization, Input Validation (SEC-FDESIGN-001..003)**: Not applicable — metric emission takes no caller input and exposes no SPI method; the single metered security surface is the `uc_timescaledb_tls_handshake_failures_total` counter (transport-level), and request-path security signals are gear-owned.
- **Integration — API (INT-FDESIGN-001)**: Not applicable — metrics are push-based over OTLP with no SPI method and no scrape endpoint.
- **Data — Persistence (DATA-FDESIGN-002, DATA-FDESIGN-004)**: Not applicable — the feature emits OpenTelemetry instruments and persists no entity, so there is no data validation, retention, or lifecycle to define.
- **Reliability — Data Integrity (REL-FDESIGN-003)**: Not applicable — instrumentation opens no write transaction and holds no consistency guarantee; the operations it measures own their own integrity (Features 2–5).
- **Usability (UX) / Compliance (COMPL)**: Not applicable — no user interface and no plugin-level regulatory obligation; inherited from Feature 1's dispositions.
- **Performance — Acceptance Criteria (PERF-FDESIGN-004)**: Not applicable as a plugin latency budget — the histograms report the other features' latencies against their NFR budgets; this feature defines no runtime p95 target of its own.
