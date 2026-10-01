# Feature: Attribution and Window Counters

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-featstatus-attribution-counters-implemented`

- [ ] `p1` - `cpt-cf-bss-rating-feature-attribution-counters`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Attribute and count a usage record](#21-attribute-and-count-a-usage-record)
  - [2.2 Apply attribution segments](#22-apply-attribution-segments)
  - [2.3 Store scope and coverage evidence](#23-store-scope-and-coverage-evidence)
  - [Migrated namespace flows](#migrated-namespace-flows)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Keep the attribution projection gap-free](#31-keep-the-attribution-projection-gap-free)
  - [3.2 Count once, withdraw once](#32-count-once-withdraw-once)
  - [3.3 Count records waiting for a usage-policy projection](#33-count-records-waiting-for-a-usage-policy-projection)
  - [3.4 Re-materialize a line layout](#34-re-materialize-a-line-layout)
  - [3.5 Enqueue composite children on input change](#35-enqueue-composite-children-on-input-change)
  - [Migrated namespace procedures](#migrated-namespace-procedures)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Attribution side of the contribution state](#41-attribution-side-of-the-contribution-state)
  - [4.2 Attribution projection per resource tenant](#42-attribution-projection-per-resource-tenant)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Exact, single-writer counting](#51-exact-single-writer-counting)
  - [5.2 Gap-free attribution projection and stored evidence](#52-gap-free-attribution-projection-and-stored-evidence)
  - [5.3 Layouts, re-materialization and composite inputs](#53-layouts-re-materialization-and-composite-inputs)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Counters and attribution: Interactions and Sequences](#counters-and-attribution-interactions-and-sequences)
  - [Counters and attribution: Re-Materialization (normative)](#counters-and-attribution-re-materialization-normative)
  - [Counters and attribution: Attribution Projection (normative)](#counters-and-attribution-attribution-projection-normative)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Attribute every normalized usage record to a subscription line through a local projection of
Subscriptions' attribution segments, place it in one **part** of its line's layout (an interval between
consecutive cut points that never crosses a UTC hour or any item's window or slice boundary), and count
it exactly once into a line-level counter keyed by the `CounterKey` and its **raw input usage type**
(T-D-76): every usage SKU
of the line sells a Products derived usage type whose inputs are raw collector GTS types, so the
counters hold inputs and `rating-core` evaluates the derived meter per granule from them. Withdraw a counted record's quantity at the coordinates it was
counted at. Store usage scopes and coverage declarations as evidence for the evaluation feature.

### 1.2 Purpose

The per-granule input quantities are the only usage input `rating-core` sees, so they must be exact, recomputable from the stored
records and serialized per counter row. Attribution comes from a projection, never from a per-record
lookup or a guessed subscription (T-D-57); a record that crosses a boundary is never split by Rating
(T-D-53); and a withdrawn record never contributes again, whatever arrives later (T-D-61).

**Requirements** (primary, per DECOMPOSITION): `cpt-cf-bss-rating-fr-tier-aggregation-window`. **Supporting**: `cpt-cf-bss-rating-fr-dimensional-pricing`, `cpt-cf-bss-rating-fr-composite-meter-eval`, `cpt-cf-bss-rating-fr-idempotency`.

**Principles**: `cpt-cf-bss-rating-principle-fail-closed`, `cpt-cf-bss-rating-principle-adopt-the-sor`, `cpt-cf-bss-rating-principle-single-writer-qst`, `cpt-cf-bss-rating-principle-versioned-q-qst`, `cpt-cf-bss-rating-principle-attribute-by-time-qst`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-rating-actor-subscriptions` | Publishes attribution segments and sealed usage scopes (TARGET, Atlas C04; PROPOSED — not implemented) |
| `cpt-cf-bss-rating-actor-oss-metering` | Declares coverage per resource through the usage emitter (owner unassigned; ABSENT) |
| `cpt-cf-bss-rating-actor-rating` | Projects segments, attributes, counts and re-materializes under its service identity |
| `cpt-cf-bss-rating-actor-platform-operator` | Releases quarantined records (audited; [Operations](11-operations.md)) |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6.4 (meter mapping, dimensions, composite meters), §6.7 (tier aggregation window).
- **Architecture**: [DESIGN.md](../DESIGN.md) §3.1 (aggregation key, the line-level `CounterKey`, child keys, expected children), §3.6 Flow A (the `count` transition and the contribution state machine), Flow B (withdrawal), §3.7 (`bss_rating__attribution_segment`, `bss_rating__usage_scope`, `bss_rating__coverage_declaration`, `bss_rating__window_counter`, `bss_rating__window_layout`, `bss_rating__usage_policy_projection` — formerly `bss_rating__meter_spec` — and `bss_rating__derived_declaration`), §4.2, §4.3, §4.5; namespace [13](../DESIGN.md#contract-13) — [counter key and windows](../DESIGN.md#contract-13-4-1), [row serialization](../DESIGN.md#contract-13-4-2), [slice layout](../DESIGN.md#contract-13-4-3), [composite inputs](../DESIGN.md#contract-13-4-5). Complete runtime procedures are in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.5.
- **Dependencies**: [Foundation](01-foundation.md) (inbox, tenancy scopes, exception register, contract fakes); [Deterministic Evaluation Core](02-evaluation-core.md) (`split_points`, window resolution, the line's cut set); [Usage Capture and Normalization](04-usage-intake.md) (normalized records, withdrawals).
- **Consumers**: [Commercial Facts and Window Scheduling](06-fact-scheduling.md) creates the expected children over these counters; [Child Evaluation and Finalization](07-child-evaluation.md) reads counters, layouts and the stored evidence; [Operations](11-operations.md) recomputes counters and releases quarantined records.
- **Upstream**: `cpt-cf-bss-rating-upreq-subscriptions-attribution-segments`, `cpt-cf-bss-rating-upreq-subscriptions-scope-proofs`, `cpt-cf-bss-rating-upreq-coverage-declarations` ([UPSTREAM_REQS](../UPSTREAM_REQS.md) §2.2, §2.4, §2.6); pricing's `UsageRatingPolicy` (window and `aggregation_scope`, T-D-75; `cpt-cf-bss-rating-upreq-pricing-aggregation-scope` is satisfied by pricing D-502/D-514, R-23 closed) and Products' derived usage declarations (T-D-76, R-31).

**UI applicability**: none; this feature has no endpoint and no user interface. The quarantine release endpoint is in [Operations](11-operations.md).

## 2. Actor Flows (CDSL)

**Use cases**: none in the PRD; the flows are system flows driven by captured usage and Subscriptions' facts.

### 2.1 Attribute and count a usage record

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-counters-count-record`

**Actor**: `cpt-cf-bss-rating-actor-rating` (page transaction, or the transaction that delivers a segment, a usage-policy projection or an operator retry)

**Success Scenarios**: the record is attributed, placed in one UTC-hour granule and one slice, counted once under its raw input usage type (`q += quantity`, `q_version + 1`); with its parent fact known, the provisional children of every usage item of the line whose derived declaration names that input are inserted if absent and enqueued.

**Error Scenarios**: no segment (`awaiting_attribution`); ambiguous segment (`quarantined`); no usage-policy projection lists the record's usage type for the line (`awaiting_spec`); interval crossing a part boundary of the line layout (a UTC hour, an item's window or slice cut such as a binding's `ends_on`) or a segment boundary (`boundary_split_required`); quantity beyond the exact bound (`rejected(quantity_out_of_range)`); record already withdrawn or counted (no effect).

**Steps**:
1. [ ] - `p1` - Look up the segment version whose interval contains the record's whole interval for `(resource_tenant_id = tenant_id, resource_id)` - `inst-cr-attribute`
2. [ ] - `p1` - **IF** none or ambiguous, set `awaiting_attribution` or `quarantined` with `withdrawn_by IS NULL` and stop - `inst-cr-defer-attr`
3. [ ] - `p1` - **IF** the interval crosses a segment boundary, set `boundary_split_required` and stop (checked before the projection, as in DESIGN §3.6 Flow A); Rating never splits a record - `inst-cr-slice`
4. [ ] - `p1` - Resolve the usage-policy projection of `(subscription, item, interval_start)` — the line's usage bindings with their `usage_rating_policy`, derived `meter` and input usage types; **IF** no binding of the line lists the record's usage type as an input, set `awaiting_spec` and stop; otherwise read the line layout head of `(counter_key_digest, period_start)` `FOR SHARE` and find the part containing the interval; **IF** the interval crosses a part boundary, set `boundary_split_required` and stop - `inst-cr-spec`
5. [ ] - `p1` - Run the `count` transition (§3.2) - `inst-cr-count`
6. [ ] - `p1` - **IF** the usage fact of `(subscription, sub_line_key, period containing the granule)` is known, then for every usage item of the line whose derived declaration names the input: insert the provisional child of the item's window (from its `rating_window`: the UTC hour, or the billing period) if absent, bump `input_generation` of the child and its window group, and enqueue it in the same transaction - `inst-cr-child`
7. [ ] - `p1` - **ELSE** keep counters only; no child exists until the fact arrives - `inst-cr-no-fact`

Canonical: DESIGN §3.6 Flow A `count(record, a, w)`; detail in [13 §3.6](#contract-13-3-6).

### 2.2 Apply attribution segments

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-counters-apply-segments`

**Actor**: `cpt-cf-bss-rating-actor-subscriptions` (segment producer, through the inbox — TARGET; PROPOSED, not implemented)

**Success Scenarios**: segment versions are applied in gap-free `lifecycle_seq` order per resource tenant; records waiting for attribution in the segment's interval are counted.

**Error Scenarios**: duplicate sequence (skipped); gap (application paused for that resource tenant, `segments_since` fill scheduled); changed owner of counted records (moved from their recorded coordinates).

**Steps**:
1. [ ] - `p1` - Accept the segment into the inbox keyed `(segment_id, segment_version)` - `inst-sg-inbox`
2. [ ] - `p1` - Lock the `segments:{resource_tenant}` checkpoint and apply the batch in `lifecycle_seq` order (§3.1) - `inst-sg-apply`
3. [ ] - `p1` - Re-attribute the resource's `awaiting_attribution` records in the segment's interval through `count` - `inst-sg-reattribute`
4. [ ] - `p1` - Advance `applied_seq` and wake children whose scope requires `segments_through_seq ≤ applied_seq` - `inst-sg-wake`
5. [ ] - `p1` - **RETURN** with the batch committed in one transaction - `inst-sg-return`

Detail: [13 §4.6](#contract-13-4-6), [13 §3.6](#contract-13-3-6).

### 2.3 Store scope and coverage evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-counters-store-evidence`

**Actor**: `cpt-cf-bss-rating-actor-subscriptions` (sealed usage scopes) and the usage emitter (coverage declarations; owner unassigned) — both TARGET; ABSENT or PROPOSED today

**Success Scenarios**: a sealed `UsageScope` or a coverage declaration version is stored once, immutably, and the children waiting on it are re-enqueued.

**Error Scenarios**: same key with a different digest (inbox quarantine); evidence that never arrives (children stay `pending`; owned by [Child Evaluation and Finalization](07-child-evaluation.md)).

**Steps**:
1. [ ] - `p1` - Accept the scope or coverage declaration into the inbox keyed `(scope_id, scope_version)` or `(coverage_id, version)` - `inst-ev-inbox`
2. [ ] - `p1` - Insert it into `bss_rating__usage_scope` or `bss_rating__coverage_declaration` (insert-only) - `inst-ev-store`
3. [ ] - `p1` - Bump `input_generation` of the children the evidence concerns and enqueue them in the same transaction - `inst-ev-wake`

The evidence gate that judges this evidence is [14 §4.5](../DESIGN.md#contract-14-4-5) and [13 §4.7](../DESIGN.md#contract-13-4-7), implemented by [Child Evaluation and Finalization](07-child-evaluation.md).

<a id="register-flows"></a>

### Migrated namespace flows

The flows of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-materialize-qst`
  — Counters and attribution — Interactions and Sequences ([contract](#contract-13-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-rematerialize-qst`
  — Counters and attribution — Interactions and Sequences ([contract](#contract-13-3-6))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-segment-apply-qst`
  — Counters and attribution — Interactions and Sequences ([contract](#contract-13-3-6))

## 3. Processes / Business Logic (CDSL)

### 3.1 Keep the attribution projection gap-free

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-counters-projection-order`

**Input**: a batch of segment versions for one `resource_tenant_id`, the checkpoint's `applied_seq`.

**Output**: an applied prefix of the batch; a paused projection on a gap.

1. [ ] - `p1` - **FOR EACH** segment in `lifecycle_seq` order - `inst-po-each`
   1. [ ] - `p1` - **IF** `lifecycle_seq ≤ applied_seq`, skip it as a duplicate - `inst-po-dup`
   2. [ ] - `p1` - **IF** `lifecycle_seq > applied_seq + 1`, keep it buffered in the inbox, stop the batch and schedule `segments_since(resource_tenant, applied_seq)` - `inst-po-gap`
   3. [ ] - `p1` - Insert the segment version (insert-only; the current version per `segment_id` is the highest) and set `applied_seq = lifecycle_seq` - `inst-po-insert`
2. [ ] - `p1` - **RETURN** the new `applied_seq` - `inst-po-return`

A gap also blocks finalization of windows whose scope requires a later sequence ([13 §4.6](#contract-13-4-6)). Orphan usage (no valid line) produces no segment; the orphan decision is Subscriptions' (Atlas P2).

### 3.2 Count once, withdraw once

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-counters-count-transition`

**Input**: a record with its attribution, its raw input usage type, the line layout version and the part; or a withdrawal returned by [Usage Capture and Normalization](04-usage-intake.md).

**Output**: one counter change at one set of coordinates, or none.

1. [ ] - `p1` - Lock the record row (`SELECT … FOR UPDATE`); **IF** it is not eligible for `counted` (state outside `captured`, `rejected`, `awaiting_attribution`, `awaiting_spec`, `boundary_split_required`, or `withdrawn_by` set), stop with no change, so an already-counted retry can never be rejected while its quantity stays in the counter. Then lock the counter row (`INSERT … (q = 0) ON CONFLICT DO NOTHING`, then `SELECT q … FOR UPDATE`; always record row first, then counter row); under both locks, **IF** `counter.q + quantity` would leave the exact bound of DESIGN §3.3, set `rejected(quantity_out_of_range)` with `withdrawn_by IS NULL`, fail the child, open an exception and stop; the SQL bound is never reached - `inst-ct-bound`
2. [ ] - `p1` - Update the record to `counted` with its recorded coordinates `WHERE withdrawn_by IS NULL AND contribution_state IN (captured, rejected, awaiting_attribution, awaiting_spec, boundary_split_required)`; **IF** no row matches, stop - `inst-ct-conditional`
3. [ ] - `p1` - Upsert the counter `(tenant_id, counter_key_digest, input_usage_type, layout_version, part_start)` (columns `part_end`, `granule_start` — the UTC hour containing the part —, `q`, `record_count`, `q_version`): `q += quantity`, `record_count += 1`, `q_version += 1`. `counter_key_digest` is the digest of the `CounterKey` (the `AggregationKey` without `item_id` and `meter`: tenant axes, subscription, `sub_line_key`, dimension value, scope) and `input_usage_type` the record's raw GTS id, so a record is counted once even when several derived meters of the line read it - `inst-ct-upsert`
4. [ ] - `p1` - **IF** a withdrawal reports a previous state `counted`, apply `q -= quantity`, `record_count -= 1`, `q_version += 1` at the **recorded** coordinates, bump the child's `input_generation` and enqueue it - `inst-ct-withdraw`
5. [ ] - `p1` - **RETURN** - `inst-ct-return`

Every state write carries `withdrawn_by IS NULL`, so deferred attribution, spec arrival, layout repair and operator retry can never count a withdrawn record.

### 3.3 Count records waiting for a usage-policy projection

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-counters-spec-arrival`

**Input**: a new `bss_rating__usage_policy_projection` row for `(subscription, item, valid_from)` — written when the line's bindings are resolved for a period (T-D-73) and the derived declaration of each usage binding is stored (`bss_rating__derived_declaration`, T-D-76).

**Output**: the `awaiting_spec` records it covers, each counted through §3.2 in the projection's transaction.

1. [ ] - `p1` - Insert the projection (insert-only): `{policy_id, version, digest, rating_window, aggregation_scope, meter, inputs}` - `inst-sa-insert`
2. [ ] - `p1` - **FOR EACH** `awaiting_spec` record whose usage type is one of the projection's inputs, run the attribution-and-count steps of §2.1 from step 3 - `inst-sa-count`
3. [ ] - `p1` - Skip any record withdrawn while it waited (its predicate matches no row) - `inst-sa-skip`

Canonical: [13 §4.2](../DESIGN.md#contract-13-4-2).

### 3.4 Re-materialize a line layout

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-counters-rematerialize`

**Input**: a trigger — the line's cut set (UTC hours plus every usage item's window bounds and slice cuts on that line: each binding's `ends_on`, term slices) differs from the stored layout, reconciliation finds `q ≠ Σ records`, an operator repair, or a segment re-attribution.

**Output**: a new layout version with counters recomputed from `counted` records.

1. [ ] - `p1` - Lock the layout head `FOR UPDATE` (counting holds `FOR SHARE`) - `inst-rm-lock`
2. [ ] - `p1` - Insert the new `bss_rating__window_layout` version — PK `(tenant_id, counter_key_digest, period_start, layout_version)` — and move the head `(tenant_id, counter_key_digest, period_start)` - `inst-rm-layout`
3. [ ] - `p1` - Insert the new version's counters by summing `counted` records per input usage type and part, each with `q_version = 1`, and rewrite those records' recorded coordinates (`contrib_layout_version`, `contrib_part_start`, `contrib_granule_start`) in the same transaction - `inst-rm-recompute`
4. [ ] - `p1` - Keep older layout versions and their counters while a retained result references them - `inst-rm-retain`
5. [ ] - `p1` - Bump `input_generation` of every child of the line in that period and enqueue them - `inst-rm-enqueue`

Same counted record set and layout give the same counters; withdrawn records never re-enter ([13 §4.4](#contract-13-4-4)).

### 3.5 Enqueue composite children on input change

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-algo-counters-composite`

**Input**: a counter change of a raw input usage type.

**Output**: the child of every derived usage meter of the line that names the input (its `AggregationKey` adds `item_id` and the derived `meter` to the `CounterKey`; the item's window) enqueued.

1. [ ] - `p1` - Look up, in the usage-policy projections of the line, the derived meters whose inputs include the changed usage type (every usage meter is derived, products P-D-259) - `inst-cp-lookup`
2. [ ] - `p1` - Bump each such child's `input_generation` and enqueue it in the counter's transaction - `inst-cp-enqueue`

Canonical: [13 §4.5](../DESIGN.md#contract-13-4-5) (Products declares the type, T-D-39).

<a id="register-procedures"></a>

### Migrated namespace procedures

The normative procedures of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-rematerialize-qst`
  — Counters and attribution — Re-Materialization (normative) ([contract](#contract-13-4-4))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-attribution-projection-qst`
  — Counters and attribution — Attribution Projection (normative) ([contract](#contract-13-4-6))

## 4. States (CDSL)

### 4.1 Attribution side of the contribution state

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-counters-contribution`

**States**: `awaiting_attribution`, `quarantined`, `awaiting_spec`, `boundary_split_required`, `counted`, `withdrawn` (this feature's transitions of the contribution state machine defined in DESIGN §3.6 Flow A; `captured`, `rejected` and withdrawal of uncounted records belong to [Usage Capture and Normalization](04-usage-intake.md)).

**Initial State**: `captured` (set by [Usage Capture and Normalization](04-usage-intake.md)).

**Transitions**:
1. [ ] - `p1` - **FROM** `captured` **TO** `awaiting_attribution`, `quarantined`, `awaiting_spec` or `boundary_split_required` **WHEN** no segment, an ambiguous segment, no usage-policy projection naming the input, or a boundary crossing is found - `inst-cs-defer`
2. [ ] - `p1` - **FROM** `captured`, `rejected`, `awaiting_*` or `boundary_split_required` **TO** `counted` **WHEN** `count` matches with `withdrawn_by IS NULL` (`+quantity` once) - `inst-cs-count`
3. [ ] - `p1` - **FROM** `awaiting_*` or `boundary_split_required` **TO** another deferred state **WHEN** a re-check finds a different missing input - `inst-cs-recheck`
4. [ ] - `p1` - **FROM** `counted` **TO** `counted` **WHEN** re-materialization moves its coordinates (net zero) - `inst-cs-move`
5. [ ] - `p1` - **FROM** `counted` **TO** `withdrawn` **WHEN** its invalidation is applied (`−quantity` once at the recorded coordinates) - `inst-cs-withdraw`
6. [ ] - `p1` - **FROM** `quarantined` **TO** `awaiting_attribution` **WHEN** an operator releases it (`usage × release`, audited) - `inst-cs-release`
7. [ ] - `p1` - **FROM** `captured` or a deferred state **TO** `rejected(quantity_out_of_range)` **WHEN** counting would leave the exact bound - `inst-cs-bound`

### 4.2 Attribution projection per resource tenant

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-counters-projection`

**States**: `applying`, `paused_gap`.

**Initial State**: `applying` (bootstrap from `segments_since`).

**Transitions**:
1. [ ] - `p1` - **FROM** `applying` **TO** `paused_gap` **WHEN** a segment arrives with `lifecycle_seq > applied_seq + 1` - `inst-ps-gap`
2. [ ] - `p1` - **FROM** `paused_gap` **TO** `applying` **WHEN** `segments_since` (or a topic seek, once a broker delivers) fills the gap - `inst-ps-fill`

## 5. Definitions of Done

### 5.1 Exact, single-writer counting

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-counters-count`

The system **MUST** count each attributed record exactly once through the conditional `count` transition, upsert the window counter with `q_version + 1` per committed change, withdraw a counted record once at its recorded coordinates, check the exact quantity bound before the upsert, and insert and enqueue the provisional child only when the parent fact is known.

**Implements**: `cpt-cf-bss-rating-flow-counters-count-record`, `cpt-cf-bss-rating-algo-counters-count-transition`, `cpt-cf-bss-rating-algo-counters-spec-arrival`, `cpt-cf-bss-rating-state-counters-contribution`, `cpt-cf-bss-rating-flow-materialize-qst`.

**Constraints**: `cpt-cf-bss-rating-constraint-core-never-writes-qst`, `cpt-cf-bss-rating-constraint-window-from-calendar-qst`, `cpt-cf-bss-rating-constraint-exact-no-rounding`.

**Touches**: `cpt-cf-bss-rating-dbtable-window-counter`, `cpt-cf-bss-rating-dbtable-meter-spec` (the usage-policy projection, renamed `bss_rating__usage_policy_projection`), `bss_rating__derived_declaration`, `cpt-cf-bss-rating-dbtable-usage-record` (contribution columns), `cpt-cf-bss-rating-dbtable-child-window` (provisional insert); queue `rating.child_work`.

### 5.2 Gap-free attribution projection and stored evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-counters-projection`

The system **MUST** apply segment versions through the inbox in gap-free `lifecycle_seq` order per resource tenant, pause and fill a gap with `segments_since`, re-attribute waiting records when a segment arrives, attribute only through a segment whose `resource_tenant_id` equals the record's tenant, and store sealed scopes and coverage declarations immutably with a wake-up of the children they concern.

**Implements**: `cpt-cf-bss-rating-flow-counters-apply-segments`, `cpt-cf-bss-rating-flow-counters-store-evidence`, `cpt-cf-bss-rating-algo-counters-projection-order`, `cpt-cf-bss-rating-state-counters-projection`, `cpt-cf-bss-rating-flow-segment-apply-qst`, `cpt-cf-bss-rating-algo-attribution-projection-qst`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-fabricated-proof-qst`, `cpt-cf-bss-rating-constraint-in-process-contracts`.

**Touches**: `cpt-cf-bss-rating-dbtable-attribution-segment` (with `bss_rating__usage_scope`, `bss_rating__coverage_declaration`), `cpt-cf-bss-rating-dbtable-inbox`, `cpt-cf-bss-rating-dbtable-feed-cursor` (`segments:{resource_tenant}`).

### 5.3 Layouts, re-materialization and composite inputs

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-counters-layout`

The system **MUST** lay out each line and fact period as parts cut at UTC hours and at every usage item's window bounds and slice cuts on that line (each binding's `ends_on`, term slices), re-materialize the line from its `counted` records under the layout-head lock when the layout changes or a repair is requested, keep layout versions referenced by retained results (`defined_by = {revision_id, binding_digest, fact_version}`), and enqueue every derived-meter child whose input counter changes. A child reads, for each input its declaration names, the parts inside its window and slice, folds them per UTC-hour granule (only `Sum` at launch), evaluates the derived formula per granule and sums.

**Implements**: `cpt-cf-bss-rating-algo-counters-rematerialize`, `cpt-cf-bss-rating-algo-counters-composite`, `cpt-cf-bss-rating-flow-rematerialize-qst`, `cpt-cf-bss-rating-algo-rematerialize-qst`.

**Constraints**: `cpt-cf-bss-rating-constraint-core-never-writes-qst`.

**Touches**: `cpt-cf-bss-rating-dbtable-window-layout`, `cpt-cf-bss-rating-dbtable-window-counter`.

Observability: this feature emits `rating_attribution_unresolved{state}`, `rating_attribution_gap{resource_tenant}`, `rating_q_materialize_seconds`, `rating_q_rematerialized_total{cause}`, `rating_usage_without_fact`, `rating_boundary_split_required_total` and the `rating.attribute` span (DESIGN §4.7).

## 6. Acceptance Criteria

- [ ] A record with no segment stays `awaiting_attribution`; when its segment arrives it is counted once.
- [ ] An ambiguous attribution is `quarantined`; after an operator release it is re-checked from `awaiting_attribution`.
- [ ] A record whose interval crosses a part boundary of its line layout (a UTC hour, an item's window or slice cut such as `ends_on`) or a segment boundary is `boundary_split_required` and not counted; the emitter's split replacement records are counted.
- [ ] A segment duplicate is skipped; a sequence gap pauses that resource tenant, `segments_since` fills it, and no window whose scope requires the missing sequence finalizes meanwhile.
- [ ] Every committed counter change raises `q_version` by exactly 1; a counter never decreases its `q_version` within a layout version.
- [ ] With the parent fact known, the first counted record of a window inserts one provisional child and enqueues it; with the fact unknown, only counters change.
- [ ] A counted record withdrawn after a re-materialization moved it is subtracted at its moved coordinates.
- [ ] Re-materializing a line layout twice from the same counted records and cut set gives identical counters; withdrawn records never re-enter.
- [ ] A change to an input counter enqueues the child of every derived usage meter of the line that names that input; the record is counted once, under the line's `CounterKey` and its raw input usage type, with exactly one contribution.
- [ ] A record of a usage type that no binding of the line names as an input stays `awaiting_spec`; it is counted when a projection naming it arrives.
- [ ] A derived declaration with a `Peak` or `TimeWeighted` input still counts its records (capture is lossless), but its child fails `unsupported_input_fold` (R-06); only `subscription_line` scope is counted at launch, `resource` scope being LAUNCH-GATED (T-D-75).

Acceptance vectors owned by this feature (DESIGN §4.13 index):

| # | Scenario | Expected state |
|---|---|---|
| V02 | Measurement captured → `awaiting_attribution` → its invalidation → attribution arrives | record `withdrawn`; contributes zero; no counter row change |
| V21 | Nine entries of `9 × 10²⁷` and a mixed-scale sum | exact `Q`; out-of-range fails `quantity_out_of_range` before persistence |

## 7. Detailed Behavior Contracts

**Contract namespace 13.** Section numbers inside the detailed contracts below resolve through the [contract address index](../DECOMPOSITION.md#4-contract-address-index), not the overview section numbers above. A bare section reference names a section of the same namespace; "DESIGN §n" names §1–§5 of [DESIGN.md](../DESIGN.md).

The following sections keep the runtime procedures of these namespaces — their interactions and sequences and the procedural parts of their §4. The flows, processes and acceptance criteria above summarize them; the architecture, schemas and evaluation semantics they rely on are defined once in [DESIGN.md](../DESIGN.md) §6.

<a id="contract-13-3-6"></a>

<!-- contract:13-q-store-attribution:3.6 -->
### Counters and attribution: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-materialize-qst` (`p1`), defined in [§2 migrated flows](#register-flows).
  **Count a record** (inside the ingestion page transaction, or the transaction that delivers its
  segment, usage-policy projection or operator retry):

  ```text
  proj = bss_rating__usage_policy_projection of (subscription, item, interval_start)
         whose inputs include record.gts_type
    none → contribution_state = awaiting_spec (WHERE withdrawn_by IS NULL); done
  head   = SELECT … FROM bss_rating__window_layout_head
           WHERE (tenant_id, counter_key_digest, period_start) = … FOR SHARE
           (insert an hour-cut layout if absent)
  part   = the part of head.current_layout_version containing the interval
                                                crosses a part boundary → boundary_split_required
                                                (every deferred-state write: WHERE withdrawn_by IS NULL)
  UPDATE bss_rating__usage_record
     SET contribution_state = counted, contrib_counter_key_digest = counter_key_digest(a),
         contrib_input_usage_type = record.gts_type,
         contrib_layout_version = head.current_layout_version, contrib_part_start = part.start,
         contrib_granule_start = the UTC hour containing part
   WHERE (tenant_id, usage_record_id) = … AND withdrawn_by IS NULL
     AND contribution_state IN (captured, rejected, awaiting_attribution, awaiting_spec,
                                boundary_split_required)
    0 rows → done                               -- withdrawn or already counted: no counter change
  INSERT INTO bss_rating__window_counter … VALUES (…, q = quantity, record_count = 1, q_version = 1)
    ON CONFLICT DO UPDATE SET q = q + excluded.q, record_count = record_count + excluded.record_count,
                              q_version = q_version + 1
  fact = usage fact of (subscription, sub_line_key, period containing part)
    known   → for each derived meter m of the line whose inputs include record.gts_type:
                INSERT bss_rating__child_window wk1|fact|window_start(m.rating_window, part)|agg_key(m)
                  (provisional) ON CONFLICT DO NOTHING
                bump input_generation of the child and of its window group; enqueue
    unknown → counters only (no child until the fact arrives, [14 §4.2](../DESIGN.md#contract-14-4-2))
  ```

  **Withdraw a record** (invalidation, same page transaction):

  ```text
  prev = UPDATE bss_rating__usage_record SET withdrawn_by = inv.id, contribution_state = withdrawn
          WHERE (tenant_id, usage_record_id) = (inv.tenant_id, inv.invalidates_id)
            AND withdrawn_by IS NULL
         RETURNING previous contribution_state, contrib_*, quantity
    0 rows, target absent      → keep the invalidation; invalidation_target_unknown; done
    0 rows, already withdrawn  → no-op
    prev = counted → counter at (contrib_counter_key_digest, contrib_input_usage_type,
                     contrib_layout_version, contrib_part_start): q -= quantity, record_count -= 1,
                     q_version + 1; bump input_generation of every child of the line whose
                     declaration names that input; enqueue them
    prev = any other state → no counter change
  ```

**Contract**: `cpt-cf-bss-rating-flow-rematerialize-qst` (`p2`), defined in [§2 migrated flows](#register-flows).
  **Re-materialize a line layout** (cut-set change or repair), one transaction: `SELECT … FOR UPDATE`
  the layout head; insert a new `bss_rating__window_layout` version and move the head; insert the
  counters of the new layout version by summing the records with `contribution_state = counted`
  (withdrawn records are excluded by construction) per input usage type and part, each with
  `q_version = 1`; update those records' recorded coordinates (`contrib_layout_version`,
  `contrib_part_start`, `contrib_granule_start`) in the same
  transaction; older layout versions and their counters stay as history while a retained result
  references them (DESIGN §4.12); bump `input_generation` of the line's children in that period; enqueue them.

**Contract**: `cpt-cf-bss-rating-flow-segment-apply-qst` (`p1`), defined in [§2 migrated flows](#register-flows).
  **Apply a segment version** (one transaction per inbox batch, per `resource_tenant_id`):

  ```text
  cp = bss_rating__source_checkpoint['segments:{resource_tenant}'] FOR UPDATE
  for s in batch ordered by lifecycle_seq:
    s.lifecycle_seq ≤ cp.applied_seq                      → duplicate, skip
    s.lifecycle_seq > cp.applied_seq + 1                  → gap: buffer in inbox, stop; schedule
                                                            segments_since(resource_tenant, applied_seq)
    INSERT bss_rating__attribution_segment (segment_id, segment_version, …) ON CONFLICT DO NOTHING
    re-attribute: records of resource_id with contribution_state = awaiting_attribution in s.interval,
                  each through count() (a withdrawn record is skipped by its predicate);
                  for a changed owner, counted records move from their recorded coordinates
                  to the new key — [contract 08](../DESIGN.md#contract-08)
    cp.applied_seq = s.lifecycle_seq
  wake children whose scope requires segments_through_seq ≤ cp.applied_seq
  COMMIT
  ```

<!-- /contract -->

<a id="contract-13-4-4"></a>

<!-- contract:13-q-store-attribution:4.4 -->
### Counters and attribution: Re-Materialization (normative)

**Contract**: `cpt-cf-bss-rating-algo-rematerialize-qst` (`p1`), defined in [§3 migrated procedures](#register-procedures).

- Triggers: the line's cut set (UTC hours, item windows and slice cuts) differs from the stored layout; reconciliation finds
  `q ≠ Σ records`; an operator repair; a segment re-attribution.
- One transaction holding `FOR UPDATE` on the layout head (counting holds `FOR SHARE`), recomputing
  every part from the `counted` rows of `bss_rating__usage_record` (hot or cold tier) and rewriting
  their recorded coordinates. Same counted record set + layout ⇒ same counters. Withdrawn records
  never re-enter, whatever the trigger.

<!-- /contract -->

<a id="contract-13-4-6"></a>

<!-- contract:13-q-store-attribution:4.6 -->
### Counters and attribution: Attribution Projection (normative)

**Contract**: `cpt-cf-bss-rating-algo-attribution-projection-qst` (`p1`), defined in [§3 migrated procedures](#register-procedures).

- Source: `AttributionSegmentChanged{segment, segments_stream_seq}` (full-state replacement, explicit
  version) and `segments_since` for gaps and bootstrap (Atlas C04, P11). Both enter the inbox
  (T-D-56).
- Order: gap-free `lifecycle_seq` per `resource_tenant_id`; a gap pauses application for that
  tenant (buffered in the inbox) and blocks finalization of windows whose scope requires a later
  sequence; Rating fills it with `segments_since` (and, when a broker exists, by seeking the topic to
  the last applied offset first).
- Lookup: the segment version whose interval contains the record's whole interval; open segments
  have `valid_to = NULL`.
- Payer transfer: one segment closes and the next opens at the same instant (Atlas F17). A record
  crossing that instant is `boundary_split_required`.
- Late `ResourceReady` for a historically valid line (Atlas F18): the segment opens at `ready_at`;
  usage held as `awaiting_attribution` is counted then through `count` — unless it was withdrawn
  meanwhile, in which case it contributes zero.
- Orphans (no valid line at `ready_at`) produce no segment; their usage stays unattributed and goes
  to the operator queue; Subscriptions owns the orphan decision (Atlas P2).

<!-- /contract -->
