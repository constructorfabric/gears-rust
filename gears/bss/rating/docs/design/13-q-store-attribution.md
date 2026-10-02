Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-02 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Windowed Counters, Attribution Projection & Scope Evidence (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md | Upstream: 12-usage-ingestion-normalization, subscriptions, IRM | Downstream: 14-unit-synthesis-period-tick | Owners: BSS Rating team -->

# DESIGN — Windowed Counters, Attribution Projection & Scope Evidence (Slice 13, pipeline)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-q-store`

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles and Constraints](#2-principles-and-constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions and Sequences](#36-interactions-and-sequences)
  - [3.7 Database Schemas and Tables](#37-database-schemas-and-tables)
  - [3.8 Deployment Topology](#38-deployment-topology)
- [4. Additional Context](#4-additional-context)
  - [4.1 Counter Key and Window Resolution (normative)](#41-counter-key-and-window-resolution-normative)
  - [4.2 Row Serialization and q_version (normative)](#42-row-serialization-and-q_version-normative)
  - [4.3 Slice Layout (normative)](#43-slice-layout-normative)
  - [4.4 Re-Materialization (normative)](#44-re-materialization-normative)
  - [4.5 Composite Inputs (normative)](#45-composite-inputs-normative)
  - [4.6 Attribution Projection (normative)](#46-attribution-projection-normative)
  - [4.7 Scope and Coverage Evidence (normative)](#47-scope-and-coverage-evidence-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice owns the state that sits between stored usage and evaluation:

1. **Window counters** — versioned per-slice quantities `Q` of one child window and one aggregation
   key. Every change increments `q_version`, so a result records exactly which quantity it priced.
   Counters are derived: each can be rebuilt from `rating_usage_record`.
2. **The attribution projection** — Rating's local copy of Subscriptions attribution segments,
   which maps `(resource, interval)` to `(subscription, sub_line_key, payer, seller)` without a call
   per record (Atlas C04, P3; T-D-52).
3. **Scope and coverage evidence** — the sealed `UsageScope` proofs and emitter coverage
   declarations the finalization gate needs (Atlas C05; T-D-47).

Ingestion places a record into a window without reading pricing through the **meter spec** — a
Rating-owned row per `(subscription, meter)` derived from the pinned plan document (the usage row's
`tierAggregationWindow` and billing anchor).

**CURRENT**: neither segments, nor scopes, nor coverage have a producer (SEAMS S-3, U-10, U-11);
the projection and evidence tables stay empty and usage stays `awaiting_attribution`
(`[DEPENDENCY GAP R-25, R-21]`).

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design response |
|---|---|
| `cpt-cf-bss-rating-fr-tier-aggregation-window` | Window boundaries from the spec; tiers reset per window (§4.1, T-D-48). |
| `cpt-cf-bss-rating-fr-single-outcome-determinism` | `q_version` + `layout_version` recorded on every result (§4.2). |
| `cpt-cf-bss-rating-fr-late-arriving-usage-reresolve` | A late record increments its slice counter and re-enqueues the child (§4.2). |
| `cpt-cf-bss-rating-fr-composite-meter-eval` | Composite children read several counters at their current versions (§4.5). |
| `cpt-cf-bss-rating-fr-level-aggregation` | Suspended (R-06); only `sum` counters are maintained. |

#### NFR Allocation

| NFR | Design response |
|---|---|
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Each counter row is its own lock; the projection is keyed per `resource_tenant_id`. |
| `cpt-cf-bss-rating-nfr-throughput-latency` | Attribution is a local indexed lookup (10M records/day forbid a remote call per record). |
| `cpt-cf-bss-rating-nfr-resilience` | Counters and projection are rebuildable; reconciliation recomputes samples daily. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-qst`

`rating` crate: `domain/window.rs` (pure window arithmetic shared with `rating-core`),
`infra/storage/counter_repo.rs`, `app/attribution_projector.rs`, `infra/storage/evidence_repo.rs`.

## 2. Principles and Constraints

### 2.1 Design Principles

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-single-writer-qst`
  Counter updates are atomic row upserts; concurrent writers serialize on the row lock. No lease is
  required for correctness.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-versioned-q-qst`
  Every change to a counter increments `q_version`; a result binds one `q_version` per slice.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-attribute-by-time-qst`
  A record belongs to the window and slice containing its whole interval; processing time never
  decides; a record that crosses a boundary is not counted (T-D-48).

### 2.2 Constraints

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-core-never-writes-qst`
  `rating-core` receives quantities in `EvaluationInput`; it never reads or writes counters.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-window-from-calendar-qst`
  Window and slice boundaries come from the spec and `rating_core::split_points` — never from
  processing time.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-no-fabricated-proof-qst`
  A scope proof is stored only as received from Subscriptions; Rating never builds one from
  current resources or from the usage it happened to see (Atlas C04).

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-qst`

- **`MeterSpec`** — `(tenant, subscription_id, meter, valid_from) → { valid_to,
  tier_aggregation_window, billing_anchor { policy, anchor_day, anchor_instant },
  aggregation_function, defined_by_catalog_version }`.
- **`AggregationKey`** — DESIGN §3.1; `agg_key_digest = SHA-256(canonical JSON)`.
- **`WindowLayout`** — ordered split points of one window + `layout_version`.
- **`SliceCounter`** — `(agg_key_digest, window_start, layout_version, slice_start) → { slice_end, q,
  record_count, q_version }`.
- **`AttributionSegment`** (Atlas C04) — `{segment_id, segment_version, resource_tenant_id,
  resource_id, usage_type, subscription_id, sub_line_key, item?, seller_tenant_id, payer_tenant_id,
  interval [from, to?), lifecycle_seq}`.
- **`UsageScope`** (Atlas C04) — `{scope_id, scope_version, billing_group_id, rating_window,
  irm_inventory_snapshot_id, irm_lifecycle_through_seq, segments_through_seq, reconciled_at, sealed,
  expected: [{resource_id, usage_type, intervals}], digest}`.
- **`CoverageDeclaration`** (Atlas C05) — `{coverage_id, version, resource_id, usage_type, period,
  intervals, final, record_count, quantity_sum, active_record_set_digest, source_checkpoint,
  supersedes_version?}`.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-q-store`

| Component | Responsibility |
|---|---|
| `CounterMaterializer::apply(tx, record, delta)` | Called by ingestion: spec → window → layout → slice; upsert counter; bump the child's `input_generation`; enqueue the child. |
| `SpecWriter` | Called by the rater after pinning: derive `MeterSpec` rows; apply records waiting for a spec. |
| `LayoutWriter` | Called by the rater: if `split_points` differ from the stored layout, re-materialize (§4.4). |
| `AttributionProjector` | Applies segment versions in `lifecycle_seq` order per `resource_tenant_id`; detects gaps; re-attributes waiting records. |
| `EvidenceStore` | Stores scopes and coverage declarations; wakes the children they concern. |

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-q-store-qst`
  Internal: `apply`, `ensure_spec`, `ensure_layout`, `read_child_quantities(child) ->
  ChildQuantities { per_slice, q_versions, layout_version }`, `attribute(resource, interval) ->
  AttributionLookup`, `evidence_for(child) -> Evidence`.
  Consumed (**PROPOSED — NOT YET IMPLEMENTED**, Atlas C04/C05): `AttributionSegmentChanged`,
  `UsageScopeSealed`, `MeterCoverageDeclared` events;
  `SubscriptionBillingReadV1::segments_since(ctx, resource_tenant_id, after_seq, limit)`,
  `SubscriptionBillingReadV1::scope_for_window(ctx, fact_id, fact_version, window,
  inventory_snapshot_id) -> Pending{required_seq} | Sealed{UsageScope}`,
  `MeterCoverageReadV1::get(ctx, resource_id, usage_type, period, version?)`.

### 3.4 Internal Dependencies

Ingestion (slice 12) calls `apply` and `attribute`; the rater (slice 14) calls `ensure_spec`,
`ensure_layout`, `read_child_quantities`, `evidence_for`.

### 3.5 External Dependencies

subscriptions (segments, scopes — SEAMS S-3, §L C04), the usage emitter (coverage — U-10; owner unassigned, overlay T6), IRM inventory
via Subscriptions' scope proof (U-11).

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-materialize-qst`
  **Apply a record** (inside the ingestion page transaction):

  ```text
  spec = rating_meter_spec covering (subscription, meter, interval_start)
    none → attribution_state = awaiting_spec; enqueue spec work; done
  w = window_of(spec, interval)                 interval not inside one window → boundary_split_required
  layout = SELECT … FROM rating_window_layout … FOR SHARE   (insert single-slice layout if absent)
  slice  = the slice containing the interval    crosses a slice → boundary_split_required
  INSERT INTO rating_window_counter … VALUES (…, q = delta, record_count = ±1, q_version = 1)
    ON CONFLICT DO UPDATE SET q = q + excluded.q, record_count = record_count + excluded.record_count,
                              q_version = q_version + 1
  UPDATE rating_usage_record SET window_start = w.start, counted = true
  fact = usage fact of (subscription, sub_line_key, period containing w)
    known   → INSERT rating_child_window wk1|fact|w.start|agg_key (provisional) ON CONFLICT DO NOTHING
              bump input_generation of the child and of its window group; enqueue
    unknown → counters only (no child until the fact arrives, slice 14 §4.2)
  ```

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-rematerialize-qst`
  **Re-materialize a window** (layout change or repair), one transaction: `SELECT … FOR UPDATE` the
  layout row; increment `layout_version`; insert the counters of the new layout version by summing
  `rating_usage_record` (counted measurements minus invalidated targets) per slice, each with
  `q_version = 1`; older layout rows stay as history; bump the child's `input_generation`; enqueue.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-segment-apply-qst`
  **Apply a segment version** (one transaction per inbox batch, per `resource_tenant_id`):

  ```text
  cp = rating_source_checkpoint['segments:{resource_tenant}'] FOR UPDATE
  for s in batch ordered by lifecycle_seq:
    s.lifecycle_seq ≤ cp.applied_seq                      → duplicate, skip
    s.lifecycle_seq > cp.applied_seq + 1                  → gap: buffer in inbox, stop; schedule
                                                            segments_since(resource_tenant, applied_seq)
    INSERT rating_attribution_segment (segment_id, segment_version, …) ON CONFLICT DO NOTHING
    re-attribute: records of resource_id with attribution_state = awaiting_attribution in s.interval
                  (and, for a changed owner, counted records under the old version — slice 08)
    cp.applied_seq = s.lifecycle_seq
  wake children whose scope requires segments_through_seq ≤ cp.applied_seq
  COMMIT
  ```

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-q-qst`
  `rating_window_counter`, `rating_window_layout`, `rating_meter_spec`,
  `rating_attribution_segment`, `rating_usage_scope`, `rating_coverage_declaration` — DESIGN §3.7.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-qst`
  No dedicated process; counters run inside ingestion and rater transactions; segment application
  runs in the inbox consumer / recovery sweep for its `resource_tenant_id`.

## 4. Additional Context

### 4.1 Counter Key and Window Resolution (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-counter-key-qst`

- Counter key `(tenant_id, agg_key_digest, window_start, layout_version, slice_start)`; the child key
  is the same plus `fact_id`, without layout and slice (T-D-04, T-D-38, T-D-48). Counters do not
  contain `fact_id`, so usage that arrives before its fact is retained and counted (F35).
- Window by `tier_aggregation_window` (UTC, half-open):

| Value | Window containing instant `t` |
|---|---|
| `per_hour` | `[t truncated to UTC hour, +1 h)` (Atlas `CalendarHour{UTC}`) |
| `calendar_month` | `[first day of t's UTC month 00:00, first day of next month)` |
| `invoice_period` | the billing period containing `t` under the spec's anchor (D-20 clamp) |
| `subscription_lifetime` | `[subscription activated_at, +∞)` — one window |
| `per_event` | no counter — the record is its own `usage_event` child |

- A `flat` usage row without `tierAggregationWindow` aggregates per `invoice_period`; for banded
  models an absent window is `missing_model_param`.
- Aggregation scope: `subscription_line` only (R-23); `resource` scope would add `resource_id` to the
  key and is not expressible in the pricing catalog today.
- `aggregation_function ≠ sum` ⇒ no counter; the child fails `unsupported_aggregation` (R-06).

### 4.2 Row Serialization and q_version (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-single-writer-qst`

- `q_version` increases by exactly 1 per committed change to a counter row; it never decreases
  within a `layout_version`.
- A result stores, per slice, the `q_version` it read; the rater skips a child when its input digest
  (which includes every slice `q_version`, the `layout_version`, the pin, the fact and subscription
  versions, the evidence refs and the engine) is unchanged.
- Records waiting for a spec are applied by `SpecWriter` in their own transaction once the spec
  exists.

### 4.3 Slice Layout (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-slice-attribution-qst`

- Split points of a child = `rating_core::split_points(window, pinned plan documents, fact version,
  subscription version)`: price-window starts/ends of the rows that price the meter, phase
  conversions and money-only term-slice boundaries inside the child's served range.
- A slice is `[split_i, split_{i+1})`; a record goes to the slice containing its whole interval.
- Band continuity across slices is computed inside `rating-core` from ordered slice quantities.
- Example: `calendar_month` window `[2026-09-01, 2026-10-01)`, billing anchor day 15, price window
  change at 2026-09-20 ⇒ two facts share the window: child A (fact of the period starting 08-15,
  served `[09-01, 09-15)`, one slice) and child B (fact of the period starting 09-15, served
  `[09-15, 10-01)`, slices `[09-15, 09-20)`, `[09-20, 10-01)`). A and B form a window group (DESIGN
  §4.3): B's graduated offset is A's billable Q; volume uses the group total for both; a counter
  change in A re-evaluates B and vice versa. Both finalize after `10-01 + delay`.
- For `per_hour` windows a price change must be on an hour boundary for tariff-shape changes (Atlas
  C10); a money-only change inside the hour is a slice with band continuity (graduated uses the
  cumulative offset, volume the final window `Q`).

### 4.4 Re-Materialization (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-rematerialize-qst`

- Triggers: the rater's `split_points` differ from the stored layout; reconciliation finds
  `q ≠ Σ records`; an operator repair; a segment re-attribution.
- One transaction holding `FOR UPDATE` on the layout row (ingestion holds `FOR SHARE`), recomputing
  every slice from `rating_usage_record` (hot or cold tier). Same record set + layout ⇒ same counters.

### 4.5 Composite Inputs (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-composite-assembly-qst`

- A composite child is keyed on its output meter; its inputs are the counters of the input meters for
  the same aggregation key except `meter`, the same window and slices, read with their `q_version`s.
  A change to any input counter enqueues the composite child (ingestion looks up composites
  referencing the meter in the spec rows).
- Composite and dimensional pricing do not co-occur at launch (R-16).

### 4.6 Attribution Projection (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-attribution-projection-qst`

- Source: `AttributionSegmentChanged{segment, segments_stream_seq}` (full-state replacement, explicit
  version) and `segments_since` for gaps and bootstrap (Atlas C04, P11). Both enter the inbox
  (T-D-51).
- Order: gap-free `lifecycle_seq` per `resource_tenant_id`; a gap pauses application for that
  tenant (buffered in the inbox) and blocks finalization of windows whose scope requires a later
  sequence; Rating fills it with `segments_since` (and, when a broker exists, by seeking the topic to
  the last applied offset first).
- Lookup: the segment version whose interval contains the record's whole interval; open segments
  have `valid_to = NULL`.
- Payer transfer: one segment closes and the next opens at the same instant (Atlas F17). A record
  crossing that instant is `boundary_split_required`.
- Late `ResourceReady` for a historically valid line (Atlas F18): the segment opens at `ready_at`;
  usage held as `awaiting_attribution` is counted then.
- Orphans (no valid line at `ready_at`) produce no segment; their usage stays unattributed and goes
  to the operator queue; Subscriptions owns the orphan decision (Atlas P2).

### 4.7 Scope and Coverage Evidence (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-scope-coverage-qst`

- When a usage child becomes due, Rating asks `scope_for_window(fact_id, fact_version, window,
  inventory_snapshot_id)` (batched per fact; the inventory snapshot id comes from IRM
  `ResourceHistoryV1::open_scope` — **UNKNOWN / EXTERNAL CONTRACT REQUIRED** whether Rating or
  Subscriptions opens it; the Atlas has Rating obtain it). `Pending{required_seq}` ⇒ child
  `pending(scope_not_sealed)`; `Sealed{UsageScope}` ⇒ stored immutably and referenced by the result.
- The expected children of the window are `expected_children(fact)` (DESIGN §3.1): one per priced
  meter of the line for `subscription_line` scope (sealed empty scope ⇒ zero children), times the
  scope's resources for `resource` scope.
- Coverage declarations are stored per `(coverage_id, version)`; a higher version supersedes and
  wakes the affected children (a correction).
- The record-set digest is computed by Rating over its stored, non-invalidated records with the
  Atlas canonical tuple and compared with the declaration; count + sum alone are never sufficient.
- A monthly coverage declaration without hourly partitions cannot finalize an hourly child (F30).

## 5. Traceability

- **DESIGN**: §3.1, §3.7, §4.3, §4.5.
- **SEAMS**: S-3, U-10, U-11, §L C04/C05, §M-3, §M-6.
- **Decisions**: T-D-04, T-D-12, T-D-38, T-D-47, T-D-48, T-D-50, T-D-52, R-06, R-16, R-21, R-23, R-25.
