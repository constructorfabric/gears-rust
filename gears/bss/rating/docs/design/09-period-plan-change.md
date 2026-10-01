Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-01 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Period & Plan-Change Obligations (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md, ../DECISIONS.md | Upstream: pricing, subscriptions | Downstream: Billing orchestration | Owners: BSS Rating team -->

# DESIGN — Period & Plan-Change Obligations (Slice 9)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-period-plan-change`

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
  - [4.1 Adopted Proration and Anchor Enums (normative)](#41-adopted-proration-and-anchor-enums-normative)
  - [4.2 PeriodFloorCapObligation Envelope (normative)](#42-periodfloorcapobligation-envelope-normative)
  - [4.3 Sub-Window Split Semantics (normative)](#43-sub-window-split-semantics-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice owns the time geometry of rating: **where a child window is cut into slices**, **how a recurring
line is prorated** over the part of a billing period it covers, and the **period floor/cap
obligation** Rating surfaces for Billing to execute.

Split points are computed by the pure function `rating_core::split_points` from the pinned pricing
documents and the subscription version ([`../DESIGN.md`](../DESIGN.md) §3.3, §4.3). A split point
is one of: a price window activation or expiry inside the child window, a phase conversion, a
money-only term-slice boundary, or (for a `period_line`) a seat change. A plan change
(`changeEffectiveAt`) and a billing-period boundary are fact boundaries: they separate children, and
a tier window spanning them is shared through a window group (DESIGN §4.3). Each
slice becomes one **line** of the child with its own `price_id` and snapshot; all slices of a child
are evaluated together, so band continuity across slices needs no cross-child coordination
(T-D-38).

Billing-period **identity** is the subscriptions commercial fact (T-D-33, `[DEPENDENCY GAP R-03]`);
period **geometry** (for window layout, proration denominators, and the MIGRATION usage parent of
R-20) is computed from the frozen `billingAnchorPolicy` with the D-20 clamp. **TARGET** (Atlas C04):
the fact carries `term_slices` that already cut the served period at every binding change; Rating
then uses the slices as split points and checks them against the geometry. The plan-change mode and its timing are the
subscriptions gear's; Rating reads `(changeEffectiveAt, changeMode)` from the subscription version
and never decides them.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-period-floor-cap-obligation` | `PeriodObligationComposer` builds a `PeriodFloorCapObligation` from the plan document's `periodFloorCaps` and carries it on the parent result and delivery; Billing executes it (§4.2, R-22). |
| `cpt-cf-bss-rating-fr-mid-cycle-proration` | A window activation inside a period is a split point; each slice is a line at full precision; a recurring line spanning the activation is prorated per the frozen `prorationBasis` (§4.3). |
| `cpt-cf-bss-rating-fr-plan-change-proration` | A plan change opens a new `sub_line_key` (`plan#n+1`), so the period has two recurring facts and two `period_line` children, each prorated over its interval; usage windows are split at `changeEffectiveAt` with counter continuity per the target plan's `usageCounterOnPlanChange` (§4.3). |

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-throughput-latency` | `split_points` | O(boundaries) per child, typically 0–2 per period; no I/O | Fixture + load test |
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Child-local geometry | Splits are computed inside one child's evaluation; no cross-child lock | Design |
| `cpt-cf-bss-rating-nfr-resilience` | Fail-closed enums | An unknown enum value or a missing required policy field fails the rating with a typed error | Conformance fixtures |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | Every slice selects its base row on the full canonical key; a split never bypasses selection. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-ppc`

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain (`rating-core`) | `AnchorCalendar`, `split_points`, proration fractions, obligation shape | Rust, pure |
| Application (pipeline) | Window layout per `split_points` (slice 13); `period_line` children from recurring facts (slice 14) | Rust gear crate |
| Infrastructure | none of its own | — |

## 2. Principles and Constraints

### 2.1 Design Principles

#### Surface, never execute

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-surface-not-execute-ppc`

Floor and cap are emitted as a structured obligation. Rating never applies `max`/`min` to a period
total, never rounds, and never posts.

#### Split, never blend

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-split-never-blend-ppc`

A boundary produces separate lines, each priced by its own row at full precision; no averaged or
blended rate exists. Billing sums the lines.

#### Consume the change, never decide it

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-consume-change-ppc`

`(changeEffectiveAt, changeMode)` come from the subscription version. The mode's only effect in
Rating is the position of the boundary.

### 2.2 Constraints

#### No rounding, no period min/max in rating-core

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-no-round-no-execute-ppc`

Line amounts leave as exact rationals (T-D-46); the invoice total is Billing's per-`invoice_line_key`
sum, rounded once under Billing's rounding policy.

#### Adopted enums, verbatim

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-enum-verbatim-ppc`

`prorationBasis ∈ {calendar_days_actual, calendar_days_30, by_second, whole_unit, none}` and
`billingAnchorPolicy ∈ {calendar_month, subscription_start, fixed_day(d)}` are decoded from the
pricing document exactly as pricing publishes them (SEAMS P-9); an unknown value fails the rating
with `unknown_enum_value`. The conformance fixtures cover every value.

#### UTC half-open boundaries only

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-utc-half-open-ppc`

All boundaries are UTC instants; every interval is half-open `[from, to)`; a boundary instant
belongs to the slice on its right.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-ppc`

- **`BillingPeriod`** — `(period_start, period_end)` of a subscription; identity from the period fact; geometry reproduced by `AnchorCalendar` for layout and denominators.
- **`SplitPoint`** — `(instant, kind)`, `kind ∈ {window_boundary, phase_conversion, plan_change, billing_period_boundary}`.
- **`Slice`** — `[from, to)` between consecutive split points inside a child window; one line per slice.
- **`ProrationFraction`** — the covered share of a billing period for a recurring line (§4.3); an exact rational, never rounded (T-D-46).
- **`PeriodFloorCapObligation`** — `{ subscription_id, period_start, period_end, kind (floor | cap), amount: ExactAmount (minor units), currency, comparison_basis, attachment_scope (usage | recurring_and_usage), plan_id, source_price_ids }` (§4.2).
- **Frozen inputs** — from the pinned plan document: `prorationBasis`, `billingAnchorPolicy`, `anchorDay`, `usageCounterOnPlanChange`, `periodFloorCaps`, window intervals; from the subscription version: plan links, phase timeline, seat timeline, `(changeEffectiveAt, changeMode)`.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-period-plan-change-ppc`

- **`AnchorCalendar`** — period boundaries from `billingAnchorPolicy` with the D-20 clamp (a day beyond the month length anchors on the month's last day; the anchor day is preserved per period: 31 → 28 → 31).
- **`SplitPointResolver`** — `split_points(child, docs, fact_version, subscription_version)`: collects window boundaries of the rows the child can select, phase conversions and plan changes from the subscription version, term-slice boundaries from the fact (TARGET), and billing-period boundaries, deduplicates coincident instants, sorts.
- **`Prorator`** — computes `ProrationFraction` per `prorationBasis`.
- **`PeriodObligationComposer`** — reads `periodFloorCaps` and builds the obligation.

### 3.3 API Contracts

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-interface-period-obligation-ppc`

**Obligation output**: `PeriodFloorCapObligation` values are stored in the `obligations` of the
plan line's window result, rolled into the parent result and delivered in
`BillableItemDeliveryV1.obligations` ([`../DESIGN.md`](../DESIGN.md) §3.3). Floor/cap *execution* is
Billing's and is Follow-on (PRD §17.4); there is no Billing gear yet (R-05). The Atlas places the
monthly minimum fee in Rating instead — CONTRACT CONFLICT (R-22).

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-interface-split-evaluation-ppc`

**Split function**: `rating_core::split_points(&SplitInput) -> Vec<OffsetDateTime>` — pure; the
pipeline uses it to lay out counter slices before evaluation (slice 13), and `evaluate` recomputes
it from the same inputs and fails with `layout_mismatch` if the counters were laid out under a
different result (the rater then re-materializes the window).
`rating_core::window_geometry(&WindowSpec, served_extent)` returns the expected child windows of a
fact (slice 14 §4.4).

### 3.4 Internal Dependencies

Slice [`01`](./01-foundation.md) (pipeline, guards), [`02`](./02-selection-eligibility.md)
(selection per slice), [`03`](./03-metering-models.md) (band continuity across slices, counter
carry/reset), [`13`](./13-q-store-attribution.md) (window layout), [`14`](./14-unit-synthesis-period-tick.md)
(`period_line` children), [`08`](./08-retroactivity-corrections.md) (re-evaluation when a boundary
appears late).

### 3.5 External Dependencies

| Dependency | What it provides | Contract |
|------------|------------------|----------|
| pricing | `prorationBasis`, `billingAnchorPolicy`/`anchorDay`, `usageCounterOnPlanChange`, `periodFloorCaps`, window intervals — in the plan document | SEAMS P-2, P-9 — `[DEPENDENCY GAP R-02]` |
| subscriptions | Plan links, phase timeline, `QuantityInterval`s, `(changeEffectiveAt, changeMode)`; facts (identity, term slices TARGET) | SEAMS S-1, S-2 — `[DEPENDENCY GAP R-03]` |
| Billing orchestration | Executes floor/cap, sums, rounds | SEAMS L-1 — `[DEPENDENCY GAP R-05]` |

### 3.6 Interactions and Sequences

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-sub-window-split-ppc`

**Mid-period plan change** (recurring line, `prorationBasis = calendar_days_actual`, September =
30 days, plan A €30.00/month, plan B €60.00/month):

1. Subscriptions records a plan change effective `2026-09-11T00:00Z`; the new subscription version closes `PlanLink plan#1` and opens `plan#2`. Two recurring facts exist for September: `lineKey = plan#1` and `lineKey = plan#2`.
2. Each fact creates a `period_line` child (slice 14); each child prorates over its interval.
3. `plan#1` covers Sep 1–10 → 10/30 × 30.00 = **10.00**; `plan#2` covers Sep 11–30 → 20/30 × 60.00 = **40.00**; total 50.00.
4. If the change were at `2026-09-11T14:00Z`, the boundary day Sep 11 belongs to the slice covering its 00:00 UTC (plan#1): 11/30 × 30.00 = **11.00** and 19/30 × 60.00 = **38.00**; the fractions still sum to 1.
5. Usage windows of the subscription are split at the same instant; the `plan#2` slice continues or resets the tier counter per plan B's `usageCounterOnPlanChange` (§4.3). For `per_hour` usage the change must fall on an hour boundary (Atlas C04/C10: tariff-shape, scope, unit, payer and window-policy changes take effect at the next UTC hour); an intra-hour change fails the child closed (`intra_window_policy_change`).

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-period-floor-cap-ppc`

**Floor/cap obligation**:

1. The plan document of the period's plan line declares a floor or cap.
2. When the plan line child is evaluated, `PeriodObligationComposer` emits the obligation with amount and currency from the document (price currency; a currency different from the line's currency fails closed — native currency only, R-07).
3. The obligation is stored with the window result and carried on the parent delivery; later revisions re-emit it deterministically; Billing applies `max(total, floor)` / `min(total, cap)` over the lines named by `attachment_scope`, after the per-line non-negative guard and after summing exact contributions.

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-none-ppc`

No table of its own. Split points are persisted by slice 13 in `rating_window_layout`; obligations
travel in `rating_window_result.obligations` and `rating_fact_result.obligations`
([`../DESIGN.md`](../DESIGN.md) §3.7).

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-ppc`

Pure `rating-core` code; no job or deployable. Rating never synthesizes a commercial period on a
calendar tick; periods come from facts (T-D-33). Rating's scheduler (slice 14) only schedules child
windows inside a fact.

## 4. Additional Context

### 4.1 Adopted Proration and Anchor Enums (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-proration-enums-ppc`

| `prorationBasis` | Fraction for a line covering `[a, b)` of period `[s, e)` |
|---|---|
| `calendar_days_actual` | covered UTC days / days in the period; the boundary day belongs to the slice covering its 00:00 UTC (T-D-26); fractions of a period sum to exactly 1 |
| `calendar_days_30` | covered days / 30, each period's total capped at 1 (a 20 + 11-day split bills 30/30) |
| `by_second` | `(b − a)` seconds / `(e − s)` seconds |
| `whole_unit` | no apportionment; a `whole_unit` line crossing a split point fails closed (`whole_unit_split_unattributed`) — `[OPEN QUESTION]` which slice bears the whole unit |
| `none` | full-period charge on the slice covering `s`; no partial credit |

- `billingAnchorPolicy`: `calendar_month` (1st, 00:00 UTC), `subscription_start` (activation
  day), `fixed_day(d)`; D-20 clamp as in §3.2. A plan change that alters the anchor policy takes
  effect from the next period boundary.
- Period identity comes from the fact; `AnchorCalendar` must reproduce it. A fact whose
  `(period_start, period_end)` differs from the calendar geometry fails the child with
  `period_geometry_mismatch` (the joint anchor fixture asserts they agree — SEAMS H-3).
- `prorationBasis` is pricing's per-price enum. The Atlas states recurring proration as "actual
  elapsed UTC seconds / full cycle seconds" (C04); that is `by_second` here and is not a default —
  the frozen enum value governs (R-17). Fractions are exact rationals (F19: one third stays 1/3).

### 4.2 PeriodFloorCapObligation Envelope (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-floor-cap-envelope-ppc`

- Rating sets amount, currency, comparison basis and attachment scope; Billing executes. Rating
  never applies the min/max and never rounds.
- Attachment: the usage lines by default, recurring + usage when the plan marks it plan-level (from
  the plan document).
- Currency: the price currency of the plan line; there is no FX at launch (R-07).
- The per-line non-negative guard runs before any period-level phase; a floor never masks a
  negative line.
- Changes arrive as re-emission on a new revision; Billing uses the obligation of the latest
  accepted revision of the plan line's fact.
- `[OPEN QUESTION]` whether a contractual floor claws back coupon discount (PRD §15; coupons are
  unavailable at launch, R-11).

### 4.3 Sub-Window Split Semantics (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-split-semantics-ppc`

- **Split points** (inside one child): window boundaries of the selectable rows; phase conversions;
  money-only term-slice boundaries; seat changes (`period_line`). Coincident instants collapse to one
  cut. Each slice is a line with its own selected row. Plan changes and billing-period boundaries
  separate children (facts).
- **Recurring lines** are prorated per §4.1. After a plan change each `sub_line_key` (`plan#n`,
  `plan#n+1`) is its own fact and `period_line` child over its interval (T-D-34).
- **Usage** is never prorated; a record belongs to the slice containing its whole interval, otherwise it is `boundary_split_required` (T-D-48).
- **Counter continuity**: inside a child, window boundaries, phase conversions and term slices
  always carry the band offset. Across facts sharing one tier window (DESIGN §4.3 window groups): a
  billing-period boundary always carries (same line, same payer); a plan change carries only when the
  **target** plan's `usageCounterOnPlanChange` is `carry` (absence = `reset`, T-D-29) **and** both rows
  match on `model_kind`, `billingGranularity`, `aggregationFunction`, `aggregationGranularity`,
  `tierAggregationWindow`, `tierQualificationWindow` and `package_size` for the shared `(meter,
  dimension_key)`; otherwise the new line starts a new group at zero and an operator signal is raised.
  A payer change always starts a new group (T-D-50). Commitment-pool carry is dormant (R-11).
- **Seat-count changes**: a `per_unit` line's quantity is read from the subscription version's seat
  timeline; a seat change inside the period is a split point of that line, each slice priced with
  its own seat count. `[OPEN QUESTION]` whether subscriptions instead opens a new `lineKey` per
  seat change (seat-change transport, carried open).
- **Cross-boundary changes**: pricing rejects in-place changes across currency, region or billing
  frequency (`crossBoundaryChangePolicy = cancel_plus_new`); Rating never receives such a split.
- **Determinism**: `split_points` and proration are pure functions of the pinned documents, the fact
  version and the subscription version; the resulting layout is recorded (`layout_version`) on the
  window result.

## 5. Traceability

**Traces to**: `cpt-cf-bss-rating-fr-period-floor-cap-obligation`, `cpt-cf-bss-rating-fr-mid-cycle-proration`, `cpt-cf-bss-rating-fr-plan-change-proration`

- **PRD**: §6.11, §17.2, AC 7, AC 17, §17.4.
- **Design**: [`../DESIGN.md`](../DESIGN.md) §3.1 (child kinds), §3.3 (`split_points`), §4.3.
- **Contracts**: [`../SEAMS.md`](../SEAMS.md) P-2, P-9, S-1, S-2, S-4, H-3.
- **Decisions**: T-D-07, T-D-26, T-D-29, T-D-33, T-D-34, T-D-38, T-D-44, T-D-46; open R-02, R-03, R-05, R-07, R-17, R-20, R-22 — [`../DECISIONS.md`](../DECISIONS.md).
- **Slices**: [`03`](./03-metering-models.md), [`13`](./13-q-store-attribution.md), [`14`](./14-unit-synthesis-period-tick.md), [`08`](./08-retroactivity-corrections.md).
