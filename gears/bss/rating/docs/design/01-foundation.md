Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-01 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Evaluation Foundation (pure-function core) (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md, ../DECISIONS.md | Upstream: pinned pricing documents, subscription versions, window counters (pipeline) | Downstream: rating pipeline (slices 14–15) | Owners: BSS Rating team -->

# DESIGN — Evaluation Foundation (pure-function core) (Slice 1)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-foundation`

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
  - [4.1 Adopted Canonical Scope Key (normative)](#41-adopted-canonical-scope-key-normative)
  - [4.2 Determinism and Idempotency Contract (normative)](#42-determinism-and-idempotency-contract-normative)
  - [4.3 Snapshot Composition (normative)](#43-snapshot-composition-normative)
  - [4.4 Emission Guards and Error Taxonomy (normative)](#44-emission-guards-and-error-taxonomy-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

The Evaluation Foundation is the `rating-core` crate's entry point and invariants. It owns the
compiled PRD §17.1 step order (steps 1–9, each step implemented by the slice that owns its
policy — 02 to 07), the `EvaluationInput` / `EvaluationOutcome` shapes, the adopted pricing scope
key, the determinism contract, the composition of the rating snapshot body, the emission guards
(non-negative line, full precision, no rounding), and the closed error taxonomy.

The core is a pure function. Everything it reads arrives in one `EvaluationInput` assembled by the
pipeline (slice 14): the pinned pricing plan and overlay documents at one `catalog_version`, the
parent fact version, one subscription version, the child window's per-slice quantities with their
`q_version`, and the `engine_version`. The core performs no I/O, reads no clock, owns no store, and
never aggregates usage. Persistence of results and snapshots is the pipeline's (slice 15;
[`../DESIGN.md`](../DESIGN.md) §3.7).

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-deterministic-evaluation-api` | `evaluate(&EvaluationInput) -> Result<EvaluationOutcome, EvaluationError>` (§3.3); replay-safe by construction (§4.2). |
| `cpt-cf-bss-rating-fr-pre-purchase-evaluation` | `evaluate_order` over an order input (no subscription version; terminal phase, `all_subscriptions`, `cohort = none`), same arithmetic, separate DTO; served by `OrderEvaluationV1::evaluate`, non-authoritative, nothing persisted (T-D-49, slice 11 §4.10; prices blocked on R-02). |
| `cpt-cf-bss-rating-fr-evaluation-order` | Steps 1–9 are called in a fixed sequence in code; there is no configuration surface that reorders them (§3.6). |
| `cpt-cf-bss-rating-fr-single-outcome-determinism` | Determinism is stated over the child window and its versioned input tuple (§4.2); the outcome carries an `input_digest`. |
| `cpt-cf-bss-rating-fr-snapshot-carry` | Every `RatedLine` carries a snapshot body and `{sku_id, plan_id, price_id}`; the pipeline stores it content-addressed (§4.3). |
| `cpt-cf-bss-rating-fr-idempotency` | The core is idempotent by purity; the pipeline's keys (usage record id, `(child_id, window_revision)`, `(fact_id, result_revision)`) are defined in [`../DESIGN.md`](../DESIGN.md) §4.2. |
| `cpt-cf-bss-rating-fr-non-negative-price` | Emission guard 1, after step 8 and before any period-level phase (§4.4). |
| `cpt-cf-bss-rating-fr-separation` | The core returns values only; it mutates nothing. A correction is a new evaluation recorded as a new revision (slice 15). |

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-throughput-latency` | `evaluate` | In-memory evaluation, O(slices × bands) for a usage window; no I/O | Benchmark in CI; targets per [`../DESIGN.md`](../DESIGN.md) §4.9 |
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Purity | No shared state; any worker can evaluate any child | Design |
| `cpt-cf-bss-rating-nfr-resilience` | Error taxonomy | Missing or inconsistent input is a typed `EvaluationError`, never a default (§4.4) | Fixture corpus + property tests |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | Adopt the pricing canonical scope key verbatim; generation by the pinned price id's cohort. |
| `cpt-cf-bss-rating-adr-rating-gear-consolidation` | `rating-core` is an I/O-free crate inside the `rating` gear. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-fnd`

```text
rating-core::pipeline (this slice)   input validation · steps 1–9 in fixed order · emission guards ·
                                     snapshot body · lineage · input digest · error taxonomy
        │ calls
        ▼
step modules (slices 02–07)          selection · metering models · overlays · reservations/commitments ·
                                     coupons · currency
        ▲ inputs (frozen, by value)
pipeline (slice 14)                  pinned PlanDocument/OverlayDocument · FactVersion ·
                                     SubscriptionVersion · ChildQuantities(q_version) · engine_version
```

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain | Pipeline, input/outcome types, key adoption, guards | `cf-gears-bss-rating-core` crate; `rust_decimal`, `time`, `uuid`, `serde` |
| Infrastructure | None | CI deny-list forbids tokio, DB, HTTP and ClientHub dependencies |

## 2. Principles and Constraints

### 2.1 Design Principles

#### Pure function, frozen inputs

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-pure-function-fnd`

No I/O, no clock, no randomness, no global state inside `rating-core`. Absence of a required input
is an `EvaluationError`, never a default. Iteration over maps uses ordered collections so output
byte order is deterministic.

#### One order, one outcome

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-one-order-fnd`

The step order is code. For one `EvaluationInput` there is exactly one `EvaluationOutcome`,
byte-identical on every worker, platform and later replay at the same `engine_version`.

#### Adopt the catalog, compose the snapshot

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-adopt-compose-fnd`

The scope key, windows, overlays, model kinds and enums are read exactly as the pinned pricing
documents carry them (§4.1). The one artifact the core composes is the snapshot body of each line
(§4.3).

### 2.2 Constraints

#### No authoritative store

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-no-store-fnd`

`rating-core` persists nothing. Counters, child windows, results, snapshots and deliveries are
pipeline stores ([`../DESIGN.md`](../DESIGN.md) §3.7).

#### UTC and exact money

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-utc-money-fnd`

All instants are UTC `OffsetDateTime`; intervals are half-open. Prices arrive from pricing as
`MinorAmount` (integer minor units, e.g. `amountMinor`, `packagePriceMinor`) and `RateMinor`
(integer 10⁻⁹ minor, e.g. `unitRateNanoMinor`, band `unitPriceNanoMinor`); quantities are finite
decimals; proration fractions are rationals. The core computes **exactly** and emits
`exact_minor: ExactAmount` — a reduced rational in minor units of the line currency (T-D-46). It
performs **no rounding at all**; Billing rounds each `invoice_line_key` aggregate once (Atlas D10).
Representation overflow is `precision_overflow`, never a truncation.

#### Result dedup owner: Rating

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-delta-dedup-owner-fnd`

Correction deduplication is Rating's (T-D-11 as amended), realized by `(child_id, window_revision)`
and `(fact_id, result_revision)` ([`../DESIGN.md`](../DESIGN.md) §4.2). The core supplies stable
`line_key`s (T-D-53) so that revisions of the same fact are comparable line by line by Billing.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-fnd`

- **`EvaluationInput`** — `child: ChildSpec` (kind, `child_id`, `RatingWindow {window_start,
  window_end, served_from, served_to}`, `AggregationKey`), `fact: FactVersion` (`fact_id,
  fact_version`, kind, timing, served extent, term slices / price ids, suspension ranges and
  posture), `catalog: PinnedCatalog` (`catalog_version`, plan documents, overlay documents),
  `subscription: SubscriptionVersion` (tenant axes, plan links, phase timeline, quantity intervals,
  `(currency, region)`, `activated_at`, pinned price ids with cohort, brand), `quantities:
  ChildQuantities` (per slice `(slice_start, slice_end, q: Decimal, q_version)`, `layout_version`;
  for `usage_event` the record quantity), `engine_version`.
- **Child kinds** — `usage_window`, `usage_event`, `period_line`, `one_time` (R-19)
  ([`../DESIGN.md`](../DESIGN.md) §3.1). A `usage_window` child is one aggregation window; its
  sub-window slices are **lines**, all evaluated in one call. Until R-19 is accepted one-time facts
  have no child (T-D-18).
- **`EvaluationOutcome`** — `lines: Vec<RatedLine>`, `obligations` (`TrueUpObligation`,
  `PeriodFloorCapObligation` — shapes in slices 05/09), `input_digest` (SHA-256 of the canonical
  serialization of the input).
- **`RatedLine`** — `line_key` (T-D-53: `{fact_id}|{meter or charge component}|{dimension_key}|
  {kind}|{gl/tax component}` — no hour, revision or price id), `slice_start`, `slice_end`, `sku_id`,
  `plan_id`, `price_id`, `charge_kind`, `model_kind`, `quantity`, `billable_quantity`, `band_offset`,
  `exact_minor: ExactAmount`, `currency`, `gl_code`, `tax_category`, `invoice_line_template`,
  `snapshot: SnapshotBody`, `lineage` (per-step pre/post amounts, applied overlay ids, band
  placement, reservation split).
- **`sub_line_key`** — Subscriptions' component-interval coordinate `lineKey` = `plan#n` /
  `addon:{addOnId}#n` (T-D-34); part of the aggregation key. It is distinct from `line_key`.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-evaluation-core-fnd`

- **`InputValidator`** — checks that every document the child needs is present at the pinned
  version and that versions are internally consistent (e.g. every price id the fact or the
  subscription references exists in the pinned plan document, and the step-2 selection returns the
  fact's `priceId` — `binding_mismatch` otherwise, R-18); fails with §4.4 errors.
- **`StepPipeline`** — calls step 1–2 (slice 02), step 3 (slice 03), steps 4–5 (slice 04), step 6
  (slice 05), step 7 (slice 06), step 8 (slice 07), then step 9 guards, per line.
- **`SplitPlanner`** — `split_points` (§3.3): computes slice boundaries of a usage window.
- **`EmissionGuard`** — non-negative guard, precision, lineage completeness (§4.4).
- **`SnapshotComposer`** — builds the snapshot body per line (§4.3).
- **`Digest`** — canonical serialization and `input_digest`.
- **`CorpusAdapter`** — implements `bss_fixtures_conformance::CorpusEvaluator` over `evaluate`,
  converting corpus band amounts (minor units) to catalog scale (10⁻⁹ minor) and comparing exact
  amounts (SEAMS H-2).

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-evaluate-fnd`

`evaluate(&EvaluationInput) -> Result<EvaluationOutcome, EvaluationError>` — the only
money-producing function. Same input ⇒ same bytes. Errors are the closed set of §4.4.

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-interface-reresolve-fnd`

**Re-evaluation.** There is no separate `reresolve` entry point: a late record, an invalidation, a
new fact or subscription version, new evidence or an administrative re-rate all lead the pipeline to
build a new `EvaluationInput` for the child and call `evaluate` again; the result is a new revision
(slice 15). No delta is computed.

`evaluate_order(&OrderEvaluationInput) -> Result<OrderEvaluation, EvaluationError>` — pre-purchase
evaluation (T-D-49, slice 11 §4.10); same step functions, separate DTO, usage excluded from TCV.

`split_points(&SplitInput) -> Vec<OffsetDateTime>` — pure; `SplitInput` = aggregation window,
pinned plan documents, subscription version, billing periods. Returns the ordered interior split
instants (slice 03 §4.3 lists the kinds). The pipeline uses it to lay out counter slices before
quantities exist (slice 13).

### 3.4 Internal Dependencies

None upstream. Slices 02–07 are modules called by `StepPipeline`; slices 08–09 define re-rating and
split/proration semantics the pipeline and `SplitPlanner` implement; slice 10 defines publish
validators and ASC 606 fields; slice 11 describes how inputs are obtained.

### 3.5 External Dependencies

| Input | Source (by value in `EvaluationInput`) | Obtained by (pipeline) |
|-------|----------------------------------------|------------------------|
| Plan and overlay documents at `catalog_version` | pricing read model | `PricingCatalogClientV1`, stored in `rating_catalog_document` ([DEPENDENCY GAP R-02]) |
| Fact version | subscriptions | `rating_fact` ([DEPENDENCY GAP R-03, R-20]) |
| Subscription version | subscriptions | `rating_subscription_version` ([DEPENDENCY GAP R-03]) |
| Per-slice quantities + `q_version` | Rating counters | `rating_window_counter` (slice 13) |
| Child window geometry | the fact's served extent + the row's `tierAggregationWindow` | slice 14 |

FX tables, coupon snapshots, contract overlays and commitment pools have no source; the core
receives none and fails closed when a line would need one ([`../DESIGN.md`](../DESIGN.md) §2.2,
R-07, R-11).

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-evaluate-line-fnd`

**Evaluate a child** (the core half of `cpt-cf-bss-rating-seq-evaluate-tariff`):

1. `InputValidator` checks presence and consistency; failure returns an error.
2. For `usage_window`: take slices from `quantities.per_slice` (already laid out by
   `split_points`); for `period_line`: one line per slice of the period (seat changes); for
   `usage_event` and `one_time`: one line.
3. Per slice/line, steps 1–2 (slice 02): active `phase_id` at the slice start, base row on the
   full scope key.
4. Step 3 (slice 03): `(meter, dimension_key)` mapping, granularity round-up, model formula with
   the in-window band offset carried across slices.
5. Steps 4–5 (slice 04), step 6 (slice 05), step 7 (slice 06), step 8 (slice 07).
6. Step 9: `EmissionGuard`, `SnapshotComposer`, lineage; compute `input_digest`.

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-reresolve-open-window-fnd`

**Re-evaluation after late usage** (core half of `cpt-cf-bss-rating-seq-correction`): the child's
counter gains `q_version + 1`; the pipeline rebuilds the input with the child's pin (provisional:
current frontier; final: pin-of-record, T-D-37) and the new quantities; `evaluate` returns the full
new outcome; the pipeline records a new revision. Whether Billing treats the next parent revision as
a draft replacement or a credit/debit note is Billing's decision (T-D-45), never the core's.

Example: a `volume` window at 70 units rated 70.00 EUR; a late 50-unit record arrives; the new
evaluation over 120 units yields 96.00 EUR (exact `9600/1` minor); the next parent revision carries
96.00 and Billing derives +26.00.

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-none-fnd`

None owned by the core. The inputs it receives are persisted by the pipeline
([`../DESIGN.md`](../DESIGN.md) §3.7); the core holds no cache.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-fnd`

`rating-core` is a library crate linked into the `rating` gear (ADR-0002) and into the fixture
conformance harness. It has no runtime of its own.

## 4. Additional Context

### 4.1 Adopted Canonical Scope Key (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-adopted-key-fnd`

Selection uses the pricing canonical scope key exactly as published in each price row's
`scopeKey` (pricing `domain/scope_key.rs`; R-10):

```text
(plan_id, currency, region, price_overlay, phase, price_eligibility, charge_kind, cohort, meter, dimension_key)
```

- "At most one row matches" holds only on the full key; hybrid `charge_kind` rows and grandfathered
  `cohort` generations are distinct keys, never an ambiguity.
- `phase` is a `phase_id` (uuid); usage rows are phase-invariant by default (terminal `phase_id`),
  a phase-specific usage row wins for its phase (slice 02 §4.3).
- `cohort ≠ none` exactly when `price_eligibility = existing_grandfathered` (pricing invariant).
- `meter` is the usage-collector GTS type id (R-09); `dimension_key` is pricing's opaque string.
  How a usage record's metadata maps onto it is undecided (R-16); at launch only the empty
  `dimension_key` is rateable (slice 03 §4.2).
- Note that pricing enforces non-overlap per `price_id` in the database, not per scope key; the
  core therefore asserts the single match itself (slice 02, `selection_ambiguity`).

### 4.2 Determinism and Idempotency Contract (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-determinism-fnd`

- **Determinism tuple**: `(catalog_version, fact_id + fact_version, subscription_version,
  per-slice q_version, layout_version, evidence refs, engine_version)` identifies an input (the
  window result records it as its manifest; counters are keyed by `layout_version`, slice 13). Every
  member refers to an immutable copy in Rating's database, so the same tuple rebuilds the same
  `EvaluationInput` and the same `input_digest` at any later time
  ([`../DESIGN.md`](../DESIGN.md) §4.1).
- **Engine version**: `engine_version` is the semver of `rating-core`. A deployment that changes
  evaluation semantics bumps it; final results are re-evaluated with a new engine only by an
  administrative re-rate (T-D-41). Replay for audit uses the recorded result's engine.
- **One pin per evaluation**: all documents in one input come from one `catalog_version`; a child
  whose slices use different price rows (a window activation inside the window) still has one pin
  — the version contains both rows.
- **Keys**: the core defines `line_key`; the pipeline defines child ids, usage dedup and revision
  keys ([`../DESIGN.md`](../DESIGN.md) §4.2).
- **No live reads inside the core**: the pipeline supplies documents for the pin it chose —
  provisional and first-final evaluations use the current frontier, corrections the pin-of-record,
  replays the recorded pin (T-D-37).

### 4.3 Snapshot Composition (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-snapshot-composition-fnd`

`SnapshotComposer` builds one `SnapshotBody` per line with the segments listed in
[`../DESIGN.md`](../DESIGN.md) §4.1 (catalog version, plan and revision, evaluation-policy version,
selected price ids with cohort and scope key, `(currency, region)`, subscription version, applied
overlay ids and revisions, empty contract/commitment/coupon/FX segments, engine version). Segments
are serialized in a fixed field order; the pipeline derives `snapshot_id = "rsnap1:" +
hex(sha256(body))`. Rating is the only writer. A body is never missing a segment: an absent source
is represented as an explicit empty segment.

### 4.4 Emission Guards and Error Taxonomy (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-emission-guards-fnd`

Applied in order at step 9, per line:

1. **Non-negative**: after step 8, a line amount below zero is a guard violation. Until Finance
   decides clamp-vs-credit (PRD §15), the guard clamps to zero and records the pre-clamp amount in
   lineage. A negative *window quantity* (net negative usage) is not clamped: it is the error
   `negative_window_quantity`.
2. **Precision**: `exact_minor` is an exact reduced rational; no rounding of any kind, no period
   floor/cap applied here (slice 09).
3. **Lineage**: pre/post amounts per step, applied ids, band placement, and the ASC 606 fields
   (`performanceObligationRef`, `sspSnapshotPointer`, null at launch — slice 10) are present on
   every line.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-error-taxonomy-fnd`

`EvaluationError` is closed; each variant becomes an exception `reason_code`
([`../DESIGN.md`](../DESIGN.md) §4.4). A child with an error gets no result.

| Error | Condition | Meaning | Slice |
|-------|-----------|---------|-------|
| `no_eligible_window` | no row on the full key covers the instant for billable usage | catalog gap for this subscription | 02 |
| `selection_ambiguity` | more than one row on the full key covers the instant | catalog non-overlap violation — pricing defect | 02 |
| `missing_phase_context` | the subscription version has no phase active at the instant | subscription data incomplete | 02 |
| `torn_cohort_pin` | grandfathered candidates exist but the subscription version has no pinned price id / cohort | subscription or migration defect | 02 |
| `cohort_pin_unmatched` | the pinned cohort matches no live generation while grandfathered candidates exist | lifecycle inconsistency; never a neighbour generation | 02 |
| `non_injective_mapping` | one `(meter, dimension_key)` maps to more than one row in the plan revision | catalog configuration error | 03 |
| `unroutable_dimension_tuple` | usage dimension values missing/partial and no published catch-all line | metering emission or catalog gap | 03 |
| `missing_model_param` | a field required by the row's `modelKind` is absent | catalog defect | 03 |
| `unknown_enum_value` | `modelKind` or another adopted enum value unknown to this `engine_version` | engine older than catalog | 03 |
| `unsupported_aggregation` | row `aggregationFunction ≠ sum` | R-06 undecided | 03 |
| `unsupported_primitive` | a primitive this engine does not implement (non-empty `dimensionKey` before R-16, `trailing_period`, allowance carry, composite × dimensions) | launch scope; pricing refuses most at publish | 03 |
| `negative_window_quantity` | a slice or window quantity is below zero | usage data defect | 03 |
| `unresolvable_scope` | an overlay scope input (e.g. `customerGroup` membership) is unavailable for the line | overlay cannot be judged; never assumed to (not) match | 04 |
| `invalid_overlay_line` | an overlay line is malformed for the priced row (e.g. unknown magnitude kind) | catalog defect | 04 |
| `contract_dimension_violation` | a contract overlay introduces an undeclared dimension | defensive (no contract source at launch) | 04 |
| `composition_cap_exceeded` | cumulative markup exceeds a `hard` `maxCumulativeMarkup` | catalog/contract configuration | 04 |
| `reservation_match_unavailable` | a reservation flavor needs a match input that has no source | no Contracts/OSS source (R-11) | 05 |
| `reservation_rate_unresolved` | a reservation applies but no reserved rate resolves from row or contract | catalog/contract defect | 05 |
| `pool_input_torn` | a line claims commitment pools but the pool set is absent or inconsistent | dormant until a Contracts source exists (R-11) | 05 |
| `coupon_source_unavailable` | the row or context references a discount/coupon | no Promotions source (R-11) | 06 |
| `coupon_policy_missing` | a coupon snapshot lacks `applyScope` or other required policy | Promotions data defect | 06 |
| `coupon_sequence_invalid` | `ordered_stack` with missing or duplicate `stackSequence` | campaign defect | 06 |
| `coupon_currency_mismatch` | a `fixed_amount` coupon's `valueCurrency` ≠ its pass currency | campaign defect | 06 |
| `coupon_scope_unsupported` | `applyScope = line_total` | no plan-scoped base (T-D-22) | 06 |
| `coupon_incompatible_pair` | two coupons marked incompatible both apply to a line | bind-time invariant violated | 06 |
| `coupon_comparison_undefined` | `exclusive_best` comparison across settlement currencies | policy undecided | 06 |
| `fx_not_supported` | billing currency ≠ selected row currency | native-currency launch (R-07) | 07 |
| `fx_rate_missing` | FX policy in force but no rate record for the conversion | dormant (R-07) | 07 |
| `fx_pair_missing` | rate table lacks the exact price→billing pair | dormant (R-07) | 07 |
| `unknown_pool_type`, `pool_currency_mismatch` | pool type not recognized; spend pool denominated in another currency than the line | dormant (R-11) | 05 |
| `meter_unpriced` | an attributed record's meter is priced by no usage row of the subscription's plans at the instant | catalog gap or wrong binding | 12 |
| `binding_mismatch` | the step-2 selection differs from the price id the fact carries | catalog/subscription divergence (R-18) | 02 |
| `intra_window_policy_change` | a tariff-shape, scope, unit, payer or window-policy change inside a `per_hour` window | upstream must align changes to the hour (Atlas C04) | 09 |
| `precision_overflow` | an exact amount exceeds the representation bound | defensive | 01 |
| `layout_mismatch` | counters were laid out under split points other than those the evaluation computes | pipeline re-materializes, then retries | 13 |
| `whole_unit_split_unattributed` | a `whole_unit` recurring line crosses a split point | attribution rule open | 09 |
| `period_geometry_mismatch` | fact period bounds differ from anchor-calendar geometry | subscriptions/Rating anchor divergence | 09 |
| `rule_violation:<id>` | a publish-time rule (validators 1–4) is violated by the pinned catalog | interim runtime enforcement (R-12) | 10 |

Pipeline **pending reasons** (`not_due`, `pin_unavailable`, `scope_not_sealed`, `segments_behind`,
`coverage_missing`, `coverage_mismatch`, `boundary_split_required`, …; list in slice 14 §3.1) are not
evaluation errors: they keep a child `pending` without calling the core (slice 14 §4.5).

## 5. Traceability

**Traces to**: `cpt-cf-bss-rating-fr-deterministic-evaluation-api`, `cpt-cf-bss-rating-fr-pre-purchase-evaluation`, `cpt-cf-bss-rating-fr-single-outcome-determinism`, `cpt-cf-bss-rating-fr-idempotency`,
`cpt-cf-bss-rating-fr-non-negative-price`, `cpt-cf-bss-rating-fr-separation`, `cpt-cf-bss-rating-fr-evaluation-order`

- **PRD**: §6.1, §6.3 `fr-evaluation-order`, §7.1, §17.1.
- **Design**: [`../DESIGN.md`](../DESIGN.md) §3.1, §3.3, §4.1, §4.2.
- **Decisions**: T-D-01, T-D-04, T-D-11, T-D-37, T-D-39, T-D-41, T-D-46, T-D-49, T-D-53, R-18 — [`../DECISIONS.md`](../DECISIONS.md).
- **Contracts**: SEAMS P-2, P-4, P-8, H-1 — [`../SEAMS.md`](../SEAMS.md).
- **ADR**: [`../ADR/0001-cpt-cf-bss-rating-adr-scope-key-adoption.md`](../ADR/0001-cpt-cf-bss-rating-adr-scope-key-adoption.md).
