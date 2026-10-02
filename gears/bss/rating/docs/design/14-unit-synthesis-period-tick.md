Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-02 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Facts, Child Windows, Scheduler & the Rater (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md | Upstream: 12, 13, subscriptions, pricing | Downstream: 15-rated-output-balance-effects | Owners: BSS Rating team -->

# DESIGN — Facts, Child Windows, Scheduler & the Rater (Slice 14, pipeline)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-unit-synthesis`

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
  - [4.1 Child Kinds (normative)](#41-child-kinds-normative)
  - [4.2 Commercial Facts (normative)](#42-commercial-facts-normative)
  - [4.3 Input Assembly (normative)](#43-input-assembly-normative)
  - [4.4 Work Queue, Scheduler and Coalescing (normative)](#44-work-queue-scheduler-and-coalescing-normative)
  - [4.5 Finalization Gate (normative)](#45-finalization-gate-normative)
  - [4.6 Commitment Balances (normative)](#46-commitment-balances-normative)
  - [4.7 Acceptance Vectors](#47-acceptance-vectors)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice decides **what is rated, when, and with which inputs**. A **commercial parent fact**
(Subscriptions) authorizes rating; Rating derives the **child windows** beneath it, schedules them
durably, evaluates each child provisionally while its window is open, and finalizes it when its
`FinalizationPolicy` delay has elapsed and its evidence is complete (T-D-44, T-D-47). The `Rater`
assembles an `EvaluationInput` from stored rows only, calls `rating-core`, and persists the result in
one transaction (DESIGN §3.6 Flow A′).

Subscriptions sends commercial facts and their changes, never a per-window timer; Rating never
invents a commercial period (T-D-33). The Atlas example is the norm: a monthly cloudlet usage fact
opens 744 hourly children in October; Rating schedules and computes each hour itself (F28, F33).

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design response |
|---|---|
| `cpt-cf-bss-rating-fr-hybrid-pricing` | Recurring and usage lines are children of distinct facts (§4.1). |
| `cpt-cf-bss-rating-fr-capacity-charge` | Capacity lines are `period_line` children (fail closed until a reservation match source exists, R-11). |
| `cpt-cf-bss-rating-fr-per-unit-pricing` | Seat quantities from the fact or the subscription version's `QuantityInterval`s, sliced at seat changes (slice 09). |
| `cpt-cf-bss-rating-fr-single-outcome-determinism` | Inputs assembled only from version-keyed stored rows; input-generation CAS (§4.3). |
| `cpt-cf-bss-rating-fr-tier-aggregation-window` | Expected child geometry from the row's `tierAggregationWindow` (§4.4). |

#### NFR Allocation

| NFR | Design response |
|---|---|
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Queue partitioned by `hash(subscription_id)`; child row lock only; scheduler sharded by `hash(fact_id)`. |
| `cpt-cf-bss-rating-nfr-throughput-latency` | Coalescing: at most one pending work item per child (§4.4); provisional evaluations are estimates and may be skipped under load. |
| `cpt-cf-bss-rating-nfr-resilience` | Leased at-least-once queue; durable schedule cursor; the rater is a no-op on unchanged inputs. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-syn`

`rating` crate: `app/fact_intake.rs`, `app/scheduler.rs`, `app/rater.rs`, `app/evidence_gate.rs`,
`infra/queue.rs` (`toolkit_db::outbox` queues `rating.child_work`, `rating.rollup`).

## 2. Principles and Constraints

### 2.1 Design Principles

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-freeze-then-invoke-syn`
  All I/O to other gears happens before the rating transaction; inside it the rater reads only
  Rating's rows and calls the pure core.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-idempotent-tick-syn`
  A fact creates its schedule and children idempotently (unique `child_id`); re-delivering a fact or
  re-running the scheduler never duplicates a child or a result.
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-cascade-off-hotpath-syn`
  A child's evaluation never evaluates another child. Cross-child effects are explicit work items:
  parent roll-up (slice 15), composite inputs (slice 13), commitment cascades (dormant, §4.6).

### 2.2 Constraints

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-pin-discipline-syn`
  The rater pins only what `PricingCatalogClientV1::pin_frontier` returns (T-D-31) for provisional
  and first-final evaluations and for administrative re-rates (which advance it), and the child's
  `pin_of_record` for every other re-evaluation (T-D-37, T-D-24).
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-synthesis-only-syn`
  This slice aggregates nothing (slice 13) and computes no money (`rating-core`).
- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-no-commercial-clock-syn`
  The scheduler only schedules windows inside a stored fact's served extent. It never creates a
  fact, a billing period, a subscription or a billing group.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-syn`

- **`FactVersion`** — `rating_fact` row: `{fact_id, fact_version, kind, parent_origin, subscription_id,
  sub_line_key, billing_group_id, tenant axes, currency, period, served extent, timing,
  occurrence_id?, price_ids, payload_digest}`.
- **`WindowSchedule`** — `{fact_id, fact_version, window_policy, finalization_policy (Rating-owned, T-D-54) {id, version,
  delay}, next_due_window_start, expected_count, scheduler_version}`; Rating-internal (Atlas C10).
- **`ChildSpec`** — `{child_id, child_kind, fact_id, window: RatingWindow {window_start, window_end,
  served_from, served_to}, aggregation_key}`; `child_id = UUIDv5(WindowEvaluationKey)` (DESIGN §3.1).
- **`WorkItem`** — `{child_id, reason}` on `rating.child_work`, partition `hash(subscription_id)`.
- **`AssembledInputs`** — `{pin, plan documents, overlay documents, fact version, subscription
  version, per-slice quantities with q_versions, layout_version, evidence refs, engine_version}` —
  exactly the material of `EvaluationInput` and of the result's input manifest.
- **`PendingReason`** — `not_due`, `pin_unavailable`, `scope_not_sealed`, `awaiting_attribution`,
  `fx_rate_pending` (dormant, R-07),
  `segments_behind`, `coverage_missing`, `coverage_mismatch`, `snapshot_unavailable`,
  `boundary_split_required`, `period_open` (parent only).

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-unit-synthesis`

| Component | Responsibility |
|---|---|
| `FactIntake` | Inbox fact → `rating_fact` (+ `rating_fact_head`), `rating_window_schedule`, `period_line` / `one_time` children; bumps `input_generation` of children affected by a new fact version. |
| `WindowScheduler` | Due scan; inserts due `usage_window` children and enqueues them; advances `next_due_window_start` in the same transaction. |
| `EvidenceGate` | Decides `final_eligible` or `pending(reason)` for a child (§4.5). |
| `Rater` | Dequeue → ensure inputs → transaction (DESIGN Flow A′) → ack. |
| `UsageParentDeriver` | **MIGRATION (R-20)**: derives a usage parent fact when Subscriptions publishes none. |

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-synthesis-syn`
  Internal: `Scheduler::enqueue(tx, child_id, reason)` must run inside the caller's transaction so
  the work item commits with its cause. External reads: `PricingCatalogClientV1` and the stored
  subscription version (slice 11). Intake of facts is the inbox (slice 11 §4.11; fact shapes §4.3).

### 3.4 Internal Dependencies

Slice 13 (`ensure_spec`, `ensure_layout`, `read_child_quantities`), slice 11 stores, slice 15
(`ResultStore::persist`, `ParentRollup`), `rating-core`.

### 3.5 External Dependencies

pricing (R-02), subscriptions (R-03, R-20), IRM/collector evidence (R-21) — SEAMS §B–§D.

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-synthesize-syn`
  **Evaluate a child** — DESIGN §3.6 Flow A′. If `split_points` computed from the assembled inputs
  differ from the stored layout, the rater calls `ensure_layout` (re-materialization, its own
  transaction), re-enqueues the child, and acks.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-period-tick-syn`
  **Fact intake** (one transaction per fact version):

  ```text
  inbox entry source=fact business_id=fact_id version=fact_version digest=d
  BEGIN
    INSERT rating_inbox … ON CONFLICT DO NOTHING            same key, digest ≠ d → quarantine; COMMIT
    INSERT rating_fact (fact_id, fact_version, …) ON CONFLICT DO NOTHING
    UPSERT rating_fact_head SET current_version = max(current_version, fact_version)
    kind = recurring → INSERT child period_line (wk1|fact|period_start|agg) ON CONFLICT DO NOTHING
    kind = one_time  → R-19 accepted ? INSERT child one_time : store only
    kind = usage     → INSERT rating_window_schedule (policy pinned now, next_due_window_start = first window)
                       INSERT provisional children for windows that already have counters (F35)
                         ON CONFLICT DO UPDATE only fact_version (policy never re-pinned)
    newer version    → UPDATE rating_child_window SET input_generation += 1 WHERE fact_id = ?
                       (children whose window intersects the changed term/served extent)
    enqueue affected children; INSERT rating_inbox state = applied
  COMMIT
  ```

  Example (recurring, MIGRATION mapping of the current Subscriptions fact): subscription S42, period
  `[2026-09-15, 2026-10-15)`, `lineKey = plan#1`, `priceId = P-REC` → `fact_id = UUIDv5(…,
  "S42|2026-09-15T00:00Z|plan#1|recurring")`, `fact_version = 1`, one `period_line` child.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-scheduler-syn`
  **Due scan** (`WindowScheduler`, every ≤ 60 s per shard, batches of ≤ 500 schedules):

  ```text
  BEGIN
    SELECT … FROM rating_window_schedule
     WHERE shard = ? AND next_due_window_start + window_length + delay <= now()
     ORDER BY next_due_window_start LIMIT 500 FOR UPDATE SKIP LOCKED
    for s: for each window w from s.next_due_window_start while w.end + s.delay <= now()
                                                      and w.start < fact.served_to:
             for each aggregation key k in expected_children(fact, w) (DESIGN §3.1):
               INSERT rating_child_window (wk1|s.fact|w.start|k) ON CONFLICT DO NOTHING
               enqueue child (reason = due)
           UPDATE s SET next_due_window_start = <first window not yet due>
  COMMIT
  ```

  After an outage the same loop catches up every due slot; the unique `child_id` absorbs re-runs
  (F28). For `subscription_line` scope the expected keys are one per meter the line's plan prices at
  the pin (empty `dimension_key` at launch), so an hour with no usage still gets its children — each
  an explicit zero once its scope is sealed (F04).

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-cascade-route-syn`
  **Wake-ups.** Children waiting on evidence are re-enqueued by the transaction that stores the
  evidence: a counter change, a fact version, a segment that raises `applied_seq` past the scope's
  `segments_through_seq`, a sealed scope, a coverage declaration. A durable timer is not needed:
  the scheduler re-enqueues children still pending after `delay` on every scan (bounded by
  `pending_rescan_interval`, default 5 min).

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-synthesis-syn`
  `rating_fact`, `rating_fact_head`, `rating_window_schedule`, `rating_child_window` — DESIGN §3.7;
  queues `rating.child_work`, `rating.rollup` in `toolkit_db::outbox` tables.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-syn`
  `Rater` workers: one processor per queue partition, N partitions (config, default 64).
  `WindowScheduler`: S shards (default 16) under leases `rating:scheduler:{shard}`.

## 4. Additional Context

### 4.1 Child Kinds (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-unit-kinds-syn`

| Kind | Parent kind | Created by | Lines | Final when |
|---|---|---|---|---|
| `usage_window` | usage | scheduler (due) or first counter write (provisional) | one per slice (slice 13 §4.3) | §4.5 |
| `usage_event` | usage | ingestion of a record under a `per_event` row | one | §4.5 (window = the record interval) |
| `period_line` | recurring | fact intake | one per slice (seat change, plan-change interval inside the period) | `advance`: at acceptance; `arrears`: when the period has ended |
| `one_time` | one_time | fact intake — only if R-19 is accepted | one | at acceptance |

The set is exhaustive. A plan change inside a period produces a new `sub_line_key` (`plan#n+1`) and
therefore a separate fact and child (T-D-34, slice 09); a seat change is a slice of the same
`period_line`.

### 4.2 Commercial Facts (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-period-tick-syn`

- The commercial WHEN is the fact (T-D-33). `fact_id` and `fact_version` are Subscriptions' (TARGET)
  or derived by the MIGRATION mapping (DESIGN Flow C, Flow usage parent).
- Fields absorbed from the fact: served extent, billing group, tenant axes, `currency`, `timing`,
  frozen price ids, suspension ranges and posture (`pause_recurring` prorates over the un-suspended
  part; `continue` bills the full period), `collection_paused` (carried to the delivery; no rating
  math). **CURRENT** Subscriptions puts suspension intervals on a fact cut at the period it
  describes; under TARGET a later suspension arrives as a new fact version.
- A new fact version invalidates the children whose window intersects a changed term slice or
  served extent; it never deletes a child: a removed obligation is a zero fact version and produces
  a zero result (Atlas C04).
- Missing fact (usage seen, no parent): the counters accumulate and **no child exists**
  (`rating_usage_without_fact`); the recovery sweep asks `period_facts` (TARGET); when the fact
  arrives, fact intake creates the children of the windows that have counters. Rating never invents
  a subscription (F35).
- `[DEPENDENCY GAP R-03, R-20]` — no fact has a wire schema or transport today.

### 4.3 Input Assembly (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-context-assembly-syn`

In order, outside the transaction:

1. `pin` = `pin_frontier` if the work reason is `admin_rerate` or the child has no `pin_of_record`,
   else `child.pin_of_record` (`pin_frontier = None` ⇒ requeue with backoff,
   `pending(pin_unavailable)`).
2. `SubscriptionVersionStore.ensure(latest)` → `subscription_version`.
3. Plans needed = the subscription's plan links active in the child's window;
   `PricingDocumentStore.ensure(catalog_version, plans + overlays)`.
4. `SpecWriter.ensure_spec(subscription, meter)` for usage children.
5. `split_points` → `ensure_layout` (usage children).

Inside the transaction: re-read the child row (`FOR UPDATE`), the fact version, counters at the
current `layout_version`, the stored documents, the subscription version, the evidence rows. The
assembled set is the input manifest; `input_digest` = SHA-256 of the canonical `EvaluationInput`.
The rater writes only if the child's `input_generation` still equals the value it read.

### 4.4 Work Queue, Scheduler and Coalescing (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-cascade-routing-syn`

- `rating.child_work`: partition `hash(subscription_id) mod N`; at-least-once; dead-letter after 10
  attempts (child `failed` + exception).
- Coalescing: enqueue is skipped when `rating_child_window.queued_at` is set; the rater clears it
  when it starts; work created during a run re-enqueues normally.
- Provisional evaluations are optional: under backlog (`rating.child_work` depth > threshold) the
  rater skips provisional work for windows not yet due; due and correction work is never skipped.
- Retry: dependency `Unavailable` / no frontier — exponential backoff 1 s → 5 min; serialization
  failure — immediate ×3; `EvaluationError` — none until an input changes or an operator retries.
- Expected geometry: `window_geometry(window_policy, served extent)`; for `per_hour` the canonical
  windows are UTC `[HH:00, HH+1:00)`; a fact opening at 10:30 has a first window 10:00–11:00 served
  from 10:30 (F29).

### 4.5 Finalization Gate (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-finalization-gate-syn`

A `usage_window` / `usage_event` child is `final_eligible` when **all** hold (Atlas C05/C10,
T-D-47):

1. the parent fact is stored (a child cannot exist without it);
2. `now ≥ window_end + finalization_policy.delay` (pinned in the schedule when the fact was
   accepted; a later configuration change never moves an in-flight deadline);
3. a sealed `UsageScope` for `(fact_id, window)` is stored (from `UsageScopeSealed` or
   `scope_for_window`), and the attribution projection's `applied_seq` for every
   `resource_tenant_id` in it is ≥ the scope's `segments_through_seq`;
4. every expected `(resource, usage type, interval)` in the scope has a `final` coverage declaration
   whose intervals cover the window;
5. the stored usage records for those resources and the window match each declaration's
   `record_count`, `quantity_sum` and `active_record_set_digest`
   (SHA-256 over sorted `(usage_record_id, window_start, window_end, quantity, unit)` of active
   records — the Atlas tuple with the collector's identity in place of the withdrawn `source_revision`);
6. no record of the window is `boundary_split_required` or `awaiting_attribution`.

A sealed empty scope (no resources) satisfies 3–5 with zero records (proven empty ⇒ zero result).
Anything missing ⇒ `pending(reason)`; never zero.

**MIGRATION (R-21)**: a finalization policy may carry `evidence_mode = delay_only`, accepted
explicitly by Finance/Product for that profile: conditions 1, 2 and 6 only. Results and deliveries
record `evidence_mode = delay_only`; late usage produces new revisions (slice 08). Without that
acceptance, usage children cannot finalize until J-13 exists.

The illustrative profiles of the Atlas are configuration, not SLAs: hourly cloudlets `delay =
PT5M`; monthly VM hours `PT48H`.

### 4.6 Commitment Balances (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-balance-freeze-syn`

Commitment pools have no source (R-11). When Contracts supplies them, pool balances enter
`EvaluationInput` as version-keyed copies and a pool change enqueues the children that observed an
older `balanceVersion` (DESIGN Flow F). Until then no child reads a balance and no
`CommitmentBalanceEffect` is published.

### 4.7 Acceptance Vectors

Copied from the Atlas (fixture ids kept) for Rating's scheduler and gate:

| Fixture | Given | Required result |
|---|---|---|
| F28 | line active all October UTC; worker down 3 h; one hour without resources | 744 children, no duplicates; empty hour = zero child; parent cannot omit a lost hour |
| F29 | activation 10:30, 8 cloudlets until 11:00 | window 10:00–11:00, served 10:30–11:00, Q = 4, 0.08 EUR |
| F30 | window ends 11:00, delay 5 min; coverage absent at 11:05, arrives 11:08 | pending at 11:05; final at/after 11:08 |
| F33 | one monthly fact, no further subscription events | every hour computed; restart resumes from checkpoints |
| F34 | 10:00–11:00 final, month open | readable via `window_results`; no parent delivery |
| F35 | usage before the fact / before attribution reaches the barrier | usage retained; fact recovered; child pending |

## 5. Traceability

- **DESIGN**: §3.1, §3.6 (Flows A′, C, usage parent), §4.1, §4.2, §4.5.
- **Decisions**: T-D-15, T-D-18, T-D-31, T-D-33, T-D-37, T-D-38, T-D-44, T-D-47, R-02, R-03, R-11,
  R-19, R-20, R-21.
- **Atlas**: C07, C10, D13, D15, F28–F35.
