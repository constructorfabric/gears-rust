# Feature: Data Retention

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
  - [1.5 Out of Scope](#15-out-of-scope)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Register the Retention Policy at Startup](#register-the-retention-policy-at-startup)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Event-Time Chunk Expiry](#event-time-chunk-expiry)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Register the declarative retention policy](#register-the-declarative-retention-policy)
  - [Make policy registration idempotent](#make-policy-registration-idempotent)
  - [Document retention-bounded key preservation](#document-retention-bounded-key-preservation)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Non-Applicable Concerns](#7-non-applicable-concerns)

<!-- /toc -->

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-featstatus-retention-implemented`

<!-- reference to DECOMPOSITION entry -->

- [ ] `p2` - `cpt-cf-uc-plugin-feature-retention`

## 1. Feature Context

### 1.1 Overview

A declarative, event-time TimescaleDB chunk-drop retention policy for `usage_records`, registered idempotently at startup and run as a backend background job, plus the retention-bounded dedup-key preservation it implies.

### 1.2 Purpose

Bounds `usage_records` storage growth for the append-heavy time-series workload without a gear-side delete path. The policy drops every chunk whose `created_at` window lies wholly outside the configured `retention_period` (default 365 days); the plugin issues no row-level delete for expiry. Retention is keyed on event time, so a row's eligibility follows its `created_at` independent of `ingested_at`. Because the dedup `UNIQUE` index rides the hypertable's chunk lifecycle, dropping a chunk also reclaims that chunk's dedup keys — which is why idempotency-key preservation is retention-bounded (chunk-granular) rather than permanent, a divergence tracked for upstream reconciliation. The `usage_type_catalog` is reference data and is not retained.

**Requirements**: `cpt-cf-uc-plugin-fr-retention`

**Constraints**: `cpt-cf-uc-plugin-constraint-retention`

**Coupled to** (owned by Feature 2, honored here): `cpt-cf-uc-plugin-constraint-dedup-key-preservation` — dropping an expired chunk reclaims that chunk's dedup keys, so the retention window bounds the dedup guarantee.

### 1.3 Actors

| Actor                                | Role in Feature                                                                    |
| ------------------------------------ | ---------------------------------------------------------------------------------- |
| `cpt-cf-uc-plugin-actor-plugin-host` | Triggers gear `init`, under which the retention policy is registered idempotently. |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **Decomposition**: `cpt-cf-uc-plugin-feature-retention`
- **Design elements**: `cpt-cf-uc-plugin-component-migrations` (registers the policy; owned by Feature 1), `cpt-cf-uc-plugin-dbtable-usage-records` (retention target; written by Feature 2)
- **Dependencies**: `cpt-cf-uc-plugin-feature-foundation`

### 1.5 Out of Scope

- Creation of the `usage_records` hypertable and the policy registration call site — created by Feature 1's Schema Migrations (`cpt-cf-uc-plugin-component-migrations`); this feature owns the policy's runtime expiry semantics and the key-preservation contract.
- The 4-tuple dedup-write behavior itself — Feature 2 (`cpt-cf-uc-plugin-feature-record-persistence`), coupled here via key preservation.
- Columnar compression of aging chunks and continuous-aggregate rollups — deferred post-v1 (see the DECOMPOSITION Deliberate Omissions and DESIGN §4).

## 2. Actor Flows (CDSL)

### Register the Retention Policy at Startup

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-flow-retention-register`

**Actor**: `cpt-cf-uc-plugin-actor-plugin-host`

**Success Scenarios**:

- The declarative retention policy is registered on the `usage_records` hypertable with the configured `retention_period`; a restart re-runs the registration as a no-op.

**Error Scenarios**:

- Registration failure aborts `init` (surfaced through Foundation's bootstrap), so the backend does not register as ready.

**Steps**:

1. [ ] - `p2` - During gear `init`, Schema Migrations resolves the configured `retention_period` (default 365 days) - `inst-ret-1`
2. [ ] - `p2` - Register the declarative TimescaleDB retention policy on the `usage_records` hypertable, keyed on the `created_at` partition column - `inst-ret-2`
3. [ ] - `p2` - **IF** the policy already exists (restart) - `inst-ret-3`
   1. [ ] - `p2` - Treat the registration as a no-op - `inst-ret-3a`
4. [ ] - `p2` - **RETURN** the policy registered; thereafter chunk expiry runs as a backend background job - `inst-ret-4`

## 3. Processes / Business Logic (CDSL)

### Event-Time Chunk Expiry

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-algo-retention-chunk-expiry`

**Input**: The `usage_records` hypertable chunks and the configured `retention_period`.

**Output**: Wholly-expired chunks dropped by the backend; their dedup keys reclaimed with them.

**Steps**:

1. [ ] - `p2` - **FOR EACH** `usage_records` chunk (evaluated by the backend background job) - `inst-exp-1`
   1. [ ] - `p2` - **IF** the chunk's `created_at` window lies wholly outside the `retention_period` - `inst-exp-1a`
      1. [ ] - `p2` - DB: DROP the chunk (no row-level delete is issued by the plugin) - `inst-exp-1a1`
      2. [ ] - `p2` - The chunk's entries in `usage_records_dedup_uniq` are reclaimed together with the chunk - `inst-exp-1a2`
2. [ ] - `p2` - A subsequent replay of a now-recordless 4-tuple is accepted as a fresh insert (retention-bounded key preservation, `cpt-cf-uc-plugin-constraint-dedup-key-preservation`) - `inst-exp-2`
3. [ ] - `p2` - The `usage_type_catalog` is reference data and is never a retention target - `inst-exp-3`

> Backfilled rows whose `created_at` is already older than the window are short-lived. Operators MUST size the retention window to exceed the maximum client replay and backfill horizon, since dedup-key preservation is bounded by it.

## 4. States (CDSL)

Not applicable — retention is a declarative backend policy operating on chunk lifecycle, not an entity state machine. A chunk is either retained or dropped by the backend job; the plugin defines no transition it drives per row.

## 5. Definitions of Done

### Register the declarative retention policy

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-retention-policy`

The system **MUST** register a declarative TimescaleDB retention policy on the `usage_records` hypertable that drops every chunk whose `created_at` window lies wholly outside the configured `retention_period` (default 365 days), and **MUST NOT** issue row-level deletes for expiry.

**Implements**:

- `cpt-cf-uc-plugin-flow-retention-register`
- `cpt-cf-uc-plugin-algo-retention-chunk-expiry`

**Constraints**: `cpt-cf-uc-plugin-constraint-retention`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-migrations`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`

### Make policy registration idempotent

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-retention-idempotent-register`

The system **MUST** register the retention policy idempotently at `init`, before serving traffic, so a restart re-runs registration as a no-op; the `usage_type_catalog` **MUST NOT** be retention-bounded.

**Implements**:

- `cpt-cf-uc-plugin-flow-retention-register`

**Constraints**: `cpt-cf-uc-plugin-constraint-retention`

**Touches**:

- Component: `cpt-cf-uc-plugin-component-migrations`
- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`

### Document retention-bounded key preservation

- [ ] `p2` - **ID**: `cpt-cf-uc-plugin-dod-retention-key-preservation-bound`

The system **MUST** treat dedup-key preservation as retention-bounded (chunk-granular, not permanent): once a chunk is dropped, a replay of its now-recordless 4-tuple is accepted as a fresh insert, and operator sizing guidance **MUST** state that the retention window exceed the maximum client replay/backfill horizon. This narrowing of the parent's unbounded-window obligation is tracked for upstream reconciliation.

**Implements**:

- `cpt-cf-uc-plugin-algo-retention-chunk-expiry`

**Constraints**: `cpt-cf-uc-plugin-constraint-dedup-key-preservation`

**Touches**:

- DB Table: `cpt-cf-uc-plugin-dbtable-usage-records`

## 6. Acceptance Criteria

- [ ] Usage-record chunks whose event-time window lies wholly outside the configured retention window are dropped by the declarative policy; no row-level expiry deletes are issued.
- [ ] The retention policy is registered idempotently at `init`; a restart re-runs registration as a no-op.
- [ ] Retention is keyed on `created_at` (event time), independent of `ingested_at`.
- [ ] The `usage_type_catalog` is not retention-bounded.
- [ ] After a chunk is dropped, a replay of its (now-recordless) 4-tuple dedup key is accepted as a fresh insert (retention-bounded preservation), and operator sizing guidance documents the replay/backfill-horizon requirement.

## 7. Non-Applicable Concerns

- **Security (SEC)**: Not applicable — retention is a declarative backend policy exposing no SPI method, no caller input, and no credential surface beyond Feature 1's TLS-required DSN; there is no request path to authenticate, authorize, or validate.
- **Integration — API (INT-FDESIGN-001)**: Not applicable — retention exposes no SPI method and no scrape/endpoint surface; it runs entirely as a backend background job.
- **Performance — Acceptance Criteria (PERF-FDESIGN-004)**: Not applicable as a plugin latency budget — chunk-drop runs asynchronously in the backend off the request path and carries no plugin-owned p95 target.
- **Recovery Procedures (REL-FDESIGN-005)**: Not applicable — chunk drop is irreversible by design and carries no compensating transaction; backup/restore and DR are governed by the operator's TimescaleDB deployment posture.
- **Usability (UX) / Compliance (COMPL)**: Not applicable — no user interface and no plugin-level regulatory obligation; inherited from Feature 1's dispositions.
- **Observability (OPS-FDESIGN-001)**: The backend background job's own signals are backend-owned; the plugin's `uc_timescaledb_dedup_stale_total` counter (Feature 6) surfaces the near-impossible retention/read-back race that this policy can cause.
