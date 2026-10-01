Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-01 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Metering & Pricing Models (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md, ../DECISIONS.md | Upstream: slice 02 outcome, window counters, pinned plan documents, subscription version | Downstream: slice 04 | Owners: BSS Rating team -->

# DESIGN — Metering & Pricing Models (Slice 3)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-metering-models`

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
  - [4.1 Model Formulas (normative)](#41-model-formulas-normative)
  - [4.2 Meter Mapping and Dimensional Lines (normative)](#42-meter-mapping-and-dimensional-lines-normative)
  - [4.3 Tier Aggregation Window, Slices and Band Continuity (normative)](#43-tier-aggregation-window-slices-and-band-continuity-normative)
  - [4.4 Granularity Round-Up (normative)](#44-granularity-round-up-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

Step 3 of the evaluation: map the child to a charge line `(meter, dimension_key)`, apply
`billingGranularity` round-up to the aggregate, and compute the model formula of the selected row
for `modelKind ∈ {flat, per_unit, graduated, volume, package}` — the pricing kind→formula mapping,
adopted verbatim. The slice also owns **slice band continuity**: when split points divide a usage
aggregation window, all slices are priced in one call, each slice's quantity placed on the bands
after the quantity of the slices before it. `hybrid` and `committed` are compositions, not kinds:
a hybrid plan emits separate `recurring` and `usage` lines; committed usage is step 6 (slice 05).

Quantities arrive computed: the pipeline materializes per-slice window quantities (slice 13) as
`rust_decimal::Decimal` from usage-collector records. This slice never aggregates records.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-meter-mapping-granularity` | `MeterMapper` (injective per plan revision) and `GranularityNormalizer` (round-up once, on the aggregate) (§4.2, §4.4). |
| `cpt-cf-bss-rating-fr-flat-pricing` | `flat` formula (§4.1). |
| `cpt-cf-bss-rating-fr-per-unit-pricing` | `per_unit` = unit rate × seat quantity from the subscription version (or `manualQuantity`), never metered Q (§4.1). |
| `cpt-cf-bss-rating-fr-tiered-graduated` | Marginal band placement with slice offset (§4.1, §4.3). |
| `cpt-cf-bss-rating-fr-volume-variant-a` | Band of the window total applied to all units (§4.1, §4.3). |
| `cpt-cf-bss-rating-fr-package-pricing` | `ceil` blocks, cumulative across slices (§4.1, §4.3). |
| `cpt-cf-bss-rating-fr-level-aggregation` | Not active: rows with `aggregationFunction ≠ sum` fail closed `unsupported_aggregation` pending R-06 (§4.3). |
| `cpt-cf-bss-rating-fr-hybrid-pricing` | Two lines under one `plan_id`, one per `charge_kind`, each selected and priced independently (§4.1). |
| `cpt-cf-bss-rating-fr-committed-usage` | Base-model math only; pool drawdown is slice 05. |
| `cpt-cf-bss-rating-fr-tier-aggregation-window` | Five window kinds with UTC boundaries (§4.3). |
| `cpt-cf-bss-rating-fr-billing-granularity` | §4.4. |
| `cpt-cf-bss-rating-fr-dimensional-pricing` | One line per `(meter, dimension_key)`; launch posture empty key only (§4.2, R-16). |
| `cpt-cf-bss-rating-fr-dimension-population-contract` | Declaration by pricing, values from usage metadata, mapping per R-16 (§4.2). |
| `cpt-cf-bss-rating-fr-composite-meter-eval` | Composite formula over ≥ 2 input quantities, then priced by the output row's `modelKind` (§3.6). |

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-throughput-latency` | `ModelFormulaEvaluator` | O(slices × bands) arithmetic, no I/O | Benchmark |
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Purity | Stateless | Design |
| `cpt-cf-bss-rating-nfr-resilience` | Fail-closed mapping and params | Typed errors (slice 01 §4.4) | Fixture corpus (tier-boundary, package, per-unit, flat, supersession-continuity families) |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | `meter` and `dimension_key` are scope-key axes of the row; the line key reuses them. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-mm`

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain | Mapping, granularity, formulas, slice continuity, composite | module `rating_core::models` |
| Infrastructure | None | — |

## 2. Principles and Constraints

### 2.1 Design Principles

#### The kind→formula mapping is shared SoR

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-shared-formula-sor-mm`

The formulas are pricing's §17.2 mapping, also encoded in the shared `bss-fixtures` corpus and its
reference oracle; Rating implements them and must agree with the oracle on every corpus row
(`CorpusEvaluator`, SEAMS H-1).

#### Price the aggregate, not the record

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-aggregate-not-record-mm`

Round-up, band placement and package blocks operate on the slice/window quantity, never on single
records (except `per_event`, whose unit is one record).

#### Never guess a line

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-never-guess-line-mm`

An ambiguous mapping, an unmapped dimension tuple, or a missing parameter is an error or an
explicitly published catch-all line — never a merged line.

### 2.2 Constraints

#### Catalog guarantees relied on

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-catalog-guarantees-mm`

Relied on as published by pricing: `graduated`/`volume`/`package` rows are `charge_kind = usage`;
the last band is open-top (`toQty = null`); bands are contiguous half-open `[fromQty, toQty)`
starting at 0; `tierQualificationWindow = trailing_period` and `includedAllowance` carry are refused
at publish (SEAMS P-10). A violation found at evaluation is `missing_model_param` or
`unsupported_primitive`, never a fallback.

#### Quantity sources are typed

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-typed-quantity-mm`

Metered quantity (window counters), seat quantity (`quantitySource = subscription_seat_count`,
from the subscription version's seat timeline) and manual quantity (`quantitySource = manual`,
`manualQuantity` on the row) are distinct; `per_unit` never reads metered quantity and usage models
never read seats.

#### Dimension declaration is not dimension emission

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-declare-vs-emit-mm`

Pricing declares the priced `dimension_key` on the row; the usage-collector carries metadata values
declared by the GTS type; the mapping between them is R-16. Rating never fabricates a value.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-mm`

- **`ChargeLineKey`** — `(meter, dimension_key)`; empty `dimension_key` for undimensioned rows.
- **`ModelParams`** — from the selected row: `modelKind`, `amountMinor` (flat), `unitRateNanoMinor`
  (flat per-unit rate / per_unit), `bands[{fromQty, toQty, unitPriceNanoMinor}]`
  (graduated/volume), `packageSize` + `packagePriceMinor` (package), `quantitySource` +
  `manualQuantity` (per_unit), `billingGranularity`, `tierAggregationWindow`,
  `aggregationFunction`, `aggregationGranularity`, `maxHoldGranules`, `reservedRateNanoMinor`,
  `reservationFlavor`.
- **`SliceQuantity`** — `(slice_start, slice_end, q: Decimal)` from the input, ordered.
- **`BandPlacement`** — per slice: offset (quantity before the slice on the band axis), band
  segments `(band, quantity, rate)`.
- **`ModelLineOutcome`** — billable quantity, placement, exact amount (`ExactAmount`, minor units,
  never rounded — T-D-46), lineage.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-metering-models-mm`

- **`MeterMapper`** — unit → `ChargeLineKey`; asserts injectivity within the plan revision.
- **`GranularityNormalizer`** — round-up per §4.4.
- **`WindowResolver`** — aggregation-window boundaries per §4.3 (also used by `split_points`).
- **`ModelFormulaEvaluator`** — §4.1 formulas with slice continuity (§4.3).
- **`CompositeMeterEvaluator`** — composite formula (§3.6).

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-price-line-mm`

`price_unit(&SelectionOutcome per slice, &ModelParams per slice, &[SliceQuantity]) ->
Result<Vec<ModelLineOutcome>, EvaluationError>` — internal. Errors: `non_injective_mapping`,
`unroutable_dimension_tuple`, `missing_model_param`, `unknown_enum_value`,
`unsupported_aggregation`, `unsupported_primitive`, `negative_window_quantity`.

### 3.4 Internal Dependencies

Upstream: slice 02 (selected rows per slice). Downstream: slice 04 (overlays on the model amount),
slice 05 (reservation split re-runs this slice over the remainder), slice 09 (split points and
recurring proration).

### 3.5 External Dependencies

| Dependency | What arrives (by value) | Contract |
|------------|------------------------|----------|
| pricing | row parameters listed in §3.1, composite declarations (`composites`) | SEAMS P-2, P-9 — [DEPENDENCY GAP R-02] |
| Rating pipeline | per-slice quantities + `q_version` | slice 13 |
| subscriptions | seat timeline | SEAMS S-1 — [DEPENDENCY GAP R-03] |

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-price-line-mm`

**Price one unit** (step 3 of `cpt-cf-bss-rating-seq-evaluate-tariff`): map the line key → check
`aggregationFunction = sum` → for each slice in order: round up, place on the slice row's bands at
the running offset, compute the amount → emit one `ModelLineOutcome` per slice.

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-composite-eval-mm`

**Composite (derived) meter**: the plan declares an output meter computed by a formula over ≥ 2
input meters (`composites` in the plan document). The pipeline materializes a composite unit whose
input is the tuple of the input meters' slice quantities for the same subscription and window,
each at its own `q_version` (slice 13). The core evaluates the formula per slice to the output
quantity and prices it by the output row's `modelKind`. Composite inputs are `sum` meters; a
composite whose inputs carry non-empty `dimension_key` is `unsupported_primitive` until the
input-join rule is decided [OPEN QUESTION — composite × dimensions, DECISIONS carried opens].

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-dimension-routing-mm`

**Dimension routing**: see §4.2 launch posture.

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-none-mm`

None.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-mm`

Part of `rating-core` (slice 01 §3.8).

## 4. Additional Context

### 4.1 Model Formulas (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-model-formulas-mm`

Prices are exact: `amountMinor` and `packagePriceMinor` are integer minor units, rates
(`unitRateNanoMinor`, band `unitPriceNanoMinor`) are integer 10⁻⁹ minor units, i.e. `rate / 10⁹`
minor. `Q` is the billable (rounded-up) quantity, a finite decimal. Every amount is an exact reduced
rational in minor units (T-D-46); nothing is rounded.

| `modelKind` | Amount | Notes |
|-------------|--------|-------|
| `flat` | recurring: `amountMinor` per period line; usage: `unitRateNanoMinor × Q` | no bands |
| `per_unit` | `unitRateNanoMinor × N`, `N` = seat quantity at the period line's start from the subscription version, or `manualQuantity` | never metered Q; seat changes mid-period are split points (slice 09) |
| `graduated` | `Σ` over bands of `(quantity in band) × unitPriceNanoMinor` | marginal |
| `volume` | `Q × unitPriceNanoMinor` of the band containing the window total | Variant A only; Variant B is not authorable |
| `package` | `ceil(Q / packageSize) × packagePriceMinor` | partial block rounds up |

- Band boundaries are half-open `[fromQty, toQty)`: a quantity exactly at `toQty` is in the next
  band, for graduated placement and for volume band selection alike.
- A single-band graduated and a single-band volume row give the same amount; `model_kind` is still
  recorded.
- A free allowance is a band with rate 0.
- Hybrid: a plan with a `recurring` and a `usage` row yields two independent lines; a "minimum
  commitment" is committed usage (slice 05), never a period floor (slice 09).

### 4.2 Meter Mapping and Dimensional Lines (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-dimensional-mapping-mm`

- A child's charge line key is `(meter, dimension_key)`; `meter` is the usage record's GTS type id
  (R-09). Within one plan revision the mapping `(meter, dimension_key) → price row` must be
  injective per `charge_kind`, else `non_injective_mapping`.
- Each distinct `(meter, dimension_key)` is its own line and its own window counter (slice 13).
- **Launch posture (R-16)**: pricing's `dimensionKey` is an opaque string and the encoding of a
  usage record's metadata into it is not yet decided. Until R-16 is decided, only rows with an
  empty `dimensionKey` are rateable; a selected row with a non-empty `dimensionKey` fails closed
  `unsupported_primitive`, and every usage record is counted under the empty `dimension_key`.
- Once R-16 is decided (proposed: `name=value` pairs of the GTS type's declared metadata, sorted by
  name, joined by `,`), a record whose values do not form a declared tuple routes to a published
  catch-all line if the plan has one, else `unroutable_dimension_tuple`.

### 4.3 Tier Aggregation Window, Slices and Band Continuity (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-tier-window-mm`

- **Windows** (`tierAggregationWindow`, UTC, half-open): `calendar_month` (first instant of the UTC
  month to the next); `invoice_period` (the billing period from `billingAnchorPolicy` with the D-20
  clamp — slice 09); `subscription_lifetime` (activation to termination); `per_hour` (UTC clock
  hour); `per_event` (no window — one `usage_event` child per record, no bands carried).
- **Split points** inside a child (computed by `split_points`, slice 01 §3.3): activation or expiry
  of a window of the selected scope key, phase conversion, money-only term-slice boundary, seat change
  (`period_line` only). Each slice is one line priced under its own selected row.
- **Band continuity**: the slices of a child share one band axis; a slice's offset is the sum of the
  billable quantities of the earlier slices. When a tier window spans several facts (billing-period
  boundary, `carry` plan change — DESIGN §4.3 window groups), the members' ordered slices form one
  band axis; a plan change with `reset` (or absent flag) starts a new axis at 0 (T-D-29).
  - `graduated` — a slice places `[offset, offset + Q_slice)` on its own row's bands.
  - `volume` — every slice uses the band that contains the **window total** (sum over all slices)
    on its own row, times its own `Q_slice`; when later usage moves the total into another band,
    the next evaluation re-prices every slice and the pipeline records a new revision.
  - `package` — a slice bills `ceil((offset + Q_slice) / size) − ceil(offset / size)` blocks at its
    own row's `packagePriceMinor`; the straddling block belongs to the slice that opened it.
- **Reservations** exclude matched quantity from the band axis window-cumulatively (T-D-23, slice 05).
- **Aggregation function**: only `sum` is active. A row with `aggregationFunction ∈ {peak,
  time_weighted}` fails closed `unsupported_aggregation` until R-06 is decided. Under option (a) of
  R-06 (not active) the window would be cut into `aggregationGranularity` granules, each folded
  (`peak` = max sample, `time_weighted` = step integral with `hold_last` bounded by
  `maxHoldGranules`), and `Q` = Σ granule folds.

Worked example — graduated, bands `[0, 100)` at 1.00 EUR, `[100, ∞)` at 0.80 EUR; window September;
a new price row (bands `[0, 100)` at 0.90 EUR, `[100, ∞)` at 0.70 EUR) activates 2026-09-15:

```text
slice A [09-01, 09-15)  Q=80   offset 0    → 80 × 1.00                 = 80.00
slice B [09-15, 10-01)  Q=50   offset 80   → 20 × 0.90 + 30 × 0.70     = 39.00
unit total 119.00 EUR; as volume: total Q=130 → A: 80 × 0.80 = 64.00, B: 50 × 0.70 = 35.00
```

**Hourly windows** (`tierAggregationWindow = per_hour`; Atlas C10/D14, T-D-48):

- The canonical window is the UTC clock hour `[HH:00, HH+1:00)`; Q resets to zero at every hour and
  nothing carries into the next hour. Hourly results roll up into the monthly parent result without
  re-selecting a tier (slice 15).
- A partial first/last hour keeps its canonical bounds with a clipped served range; the quantity is
  the actual source-integrated quantity and **thresholds are not prorated**.
- Volume selects the band containing the whole-hour Q and applies its rate to all Q of that hour;
  graduated allocates Q across bands from zero within the hour.
- Aggregation scope is `subscription_line` (all attributed resources of one subscription line, item,
  dimension value and tenant axes share Q); `resource` scope is not expressible in the catalog today
  (R-23).
- Hourly rows with a period floor (`periodFloorCaps`) or an included allowance are not rateable per
  hour: a floor is a period obligation (slice 09) and never applied per hour; an allowance on a
  `per_hour` row fails closed `unsupported_primitive` (Atlas C10: "do not apply a monthly floor once
  per hour").

Worked examples — bands `[0, 10)` €0.02, `[10, ∞)` €0.015 (Atlas fixtures):

| Fixture | Input | Volume | Graduated |
|---|---|---|---|
| F23 | hours Q = 8, then Q = 12 | 0.16 + 0.18 = **0.34** (one tier over Q = 20 would give 0.30 — forbidden) | 0.16 + 0.23 = 0.39 |
| F24 | one hour Q = 10 | 10 × 0.015 = 0.15 | 10 × 0.02 = 0.20 |
| F24 | one hour Q = 12 | 0.18 | 10 × 0.02 + 2 × 0.015 = 0.23 |
| F25 | two resources × 6 in one hour, one line | Q = 12 → 0.18 (resource scope would give 0.12 + 0.12 = 0.24) | 0.23 |
| F29 | activation 10:30, 8 cloudlets until 11:00 | Q = 4 → 0.08 | 0.08 |

### 4.4 Granularity Round-Up (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-granularity-mm`

- `billingGranularity ∈ {per_second, per_minute, per_hour, per_day, whole_unit}` rounds the slice
  (or `per_event` record) quantity **up** to a whole multiple of the unit, once, before band
  placement. Example: a `per_hour` meter measured in seconds with twelve 5-minute records in one
  hour has slice quantity 3 600 s → 1 hour, not 12 hours.
- Band offsets use billable (rounded) quantities, so slice continuity is on the same scale as the
  bands.
- The applied granularity is recorded in lineage. A per-resource `minimumCharge` is not modelled
  until pricing publishes it [OPEN QUESTION — PRD §15 minimum charge].

## 5. Traceability

**Traces to**: `cpt-cf-bss-rating-fr-flat-pricing`, `cpt-cf-bss-rating-fr-per-unit-pricing`, `cpt-cf-bss-rating-fr-tiered-graduated`,
`cpt-cf-bss-rating-fr-volume-variant-a`, `cpt-cf-bss-rating-fr-package-pricing`, `cpt-cf-bss-rating-fr-level-aggregation`,
`cpt-cf-bss-rating-fr-hybrid-pricing`, `cpt-cf-bss-rating-fr-meter-mapping-granularity`, `cpt-cf-bss-rating-fr-tier-aggregation-window`,
`cpt-cf-bss-rating-fr-billing-granularity`, `cpt-cf-bss-rating-fr-dimensional-pricing`, `cpt-cf-bss-rating-fr-composite-meter-eval`

- **PRD**: §6.2, §6.3 `fr-meter-mapping-granularity`, §6.5, §6.7, §17.1 step 3.
- **Design**: [`../DESIGN.md`](../DESIGN.md) §4.3.
- **Contracts**: SEAMS P-2, P-9, P-10, U-4, H-1, §I (M1–M7, M10–M11) — [`../SEAMS.md`](../SEAMS.md).
- **Decisions**: T-D-05, T-D-12, T-D-13, T-D-23, T-D-26, T-D-29, T-D-38, T-D-46, T-D-48, R-06, R-09, R-16, R-23 — [`../DECISIONS.md`](../DECISIONS.md).
- **Related slices**: [`01-foundation.md`](./01-foundation.md), [`02-selection-eligibility.md`](./02-selection-eligibility.md), [`05-commitments-reservations.md`](./05-commitments-reservations.md), [`09-period-plan-change.md`](./09-period-plan-change.md), [`13-q-store-attribution.md`](./13-q-store-attribution.md).
