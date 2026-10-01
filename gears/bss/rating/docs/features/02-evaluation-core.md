# Feature: Deterministic Evaluation Core

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-featstatus-evaluation-core-implemented`

- [ ] `p1` - `cpt-cf-bss-rating-feature-evaluation-core`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Evaluate one child input](#21-evaluate-one-child-input)
  - [2.2 Check the rules that remain at evaluation](#22-check-the-rules-that-remain-at-evaluation)
  - [Migrated namespace flows](#migrated-namespace-flows)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Run the fixed step pipeline](#31-run-the-fixed-step-pipeline)
  - [3.2 Take the bound price and check the binding](#32-take-the-bound-price-and-check-the-binding)
  - [3.3 Price a line by its model](#33-price-a-line-by-its-model)
  - [3.4 Compute split points, window geometry, proration and the minimum-fee floor](#34-compute-split-points-window-geometry-proration-and-the-minimum-fee-floor)
  - [3.5 Exact arithmetic under a budget](#35-exact-arithmetic-under-a-budget)
  - [3.6 Release and retain engine generations](#36-release-and-retain-engine-generations)
  - [3.7 Pass through governance evidence](#37-pass-through-governance-evidence)
  - [3.8 Evaluate an order input](#38-evaluate-an-order-input)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Engine generation lifecycle](#41-engine-generation-lifecycle)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Pure evaluation entry points](#51-pure-evaluation-entry-points)
  - [5.2 Bindings, models, windows and period math](#52-bindings-models-windows-and-period-math)
  - [5.3 Exact numbers and retained engine generations](#53-exact-numbers-and-retained-engine-generations)
  - [5.4 Governance pass-through and defensive rules](#54-governance-pass-through-and-defensive-rules)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Core foundation: Interactions and Sequences](#core-foundation-interactions-and-sequences)
  - [Selection: Interactions and Sequences](#selection-interactions-and-sequences)
  - [Metering models: Interactions and Sequences](#metering-models-interactions-and-sequences)
  - [Period and plan change: Interactions and Sequences](#period-and-plan-change-interactions-and-sequences)
  - [Governance: Interactions and Sequences](#governance-interactions-and-sequences)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Build `rating-core`: the pure, I/O-free crate that turns one frozen `EvaluationInput` into one
`EvaluationOutcome` — the fixed step order re-cut against the PriceBook pricing model (T-D-73:
bind, meter, model formula, minimum-fee floor at roll-up, guards; DESIGN namespace 01), period math
(proration T-D-77), exact arithmetic under a checked budget, the closed error taxonomy, and the
retained engine generations that let any recorded result be executed again. Prices arrive as
**bindings** that Pricing's `resolve` returned for the subscription's pins; the core never selects
a price. It also provides `evaluate_order`, the pure function behind order evaluation.

### 1.2 Purpose

Every rated amount in Rating is this crate's output for a frozen input. Determinism, exactness and
replay are therefore properties of one crate that owns no store and reads no clock, and they can be
built and proven against the shared fixture corpus before any upstream contract exists (DECOMPOSITION
build step 1).

**Requirements** (primary, per DECOMPOSITION): `cpt-cf-bss-rating-fr-deterministic-evaluation-api`, `cpt-cf-bss-rating-fr-single-outcome-determinism`, `cpt-cf-bss-rating-fr-non-negative-price`, `cpt-cf-bss-rating-fr-evaluation-order`, `cpt-cf-bss-rating-fr-base-catalog-selection` (selection superseded by T-D-73), `cpt-cf-bss-rating-fr-price-eligibility-grandfathering` (superseded by T-D-73), `cpt-cf-bss-rating-fr-plan-phases` (superseded by T-D-73), `cpt-cf-bss-rating-fr-flat-pricing`, `cpt-cf-bss-rating-fr-per-unit-pricing`, `cpt-cf-bss-rating-fr-tiered-graduated`, `cpt-cf-bss-rating-fr-volume-variant-a`, `cpt-cf-bss-rating-fr-package-pricing`, `cpt-cf-bss-rating-fr-hybrid-pricing`, `cpt-cf-bss-rating-fr-level-aggregation`, `cpt-cf-bss-rating-fr-meter-mapping-granularity`, `cpt-cf-bss-rating-fr-billing-granularity`, `cpt-cf-bss-rating-fr-dimensional-pricing`, `cpt-cf-bss-rating-fr-composite-meter-eval`, `cpt-cf-bss-rating-fr-period-floor-cap-obligation`, `cpt-cf-bss-rating-fr-mid-cycle-proration`, `cpt-cf-bss-rating-fr-plan-change-proration`, `cpt-cf-bss-rating-fr-asc606-traceable-identifiers`, `cpt-cf-bss-rating-fr-publish-approval-governance`, `cpt-cf-bss-rating-nfr-audit-segregation`, `cpt-cf-bss-rating-interface-tariff-evaluation`.

**Principles**: `cpt-cf-bss-rating-principle-pure-function-core`, `cpt-cf-bss-rating-principle-fixed-rule-order`, `cpt-cf-bss-rating-principle-adopt-the-sor`, `cpt-cf-bss-rating-principle-fail-closed`, `cpt-cf-bss-rating-principle-pure-function-fnd`, `cpt-cf-bss-rating-principle-shared-formula-sor-mm`, `cpt-cf-bss-rating-principle-split-never-blend-ppc`, `cpt-cf-bss-rating-principle-pass-through-gov`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-rating-actor-rating` | Calls the core in-process with a frozen input, under the engine generation the input names |
| `cpt-cf-bss-rating-actor-catalog` | Pricing: owns price books, entries, prices, plan revisions and each usage entry's `UsageRatingPolicy`; its `resolve` binds the price the core evaluates (pricing D-419, D-420) |
| `cpt-cf-bss-rating-actor-subscriptions` | Owns the period, the pins (accepted price bindings, SUB-D-29), the committed quantity and plan changes carried in the input |
| `cpt-cf-bss-rating-actor-product-manager` | Authors books, prices and plans in Pricing and derived usage types in Products; Pricing runs its own publish checks, so the core's verdicts surface only as rating exceptions |
| `cpt-cf-bss-rating-actor-platform-operator` | Relies on replay of retained engine generations for audit |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6 (evaluation requirements), §17.1 (step order).
- **Architecture**: [DESIGN.md](../DESIGN.md) §3.1 (child kinds, slices), §3.3 (`rating-core` API, exact numbers), §4.1 (binding of record, engine generations), §4.3 (aggregation semantics, worked examples), §4.10 (capability matrix); namespaces [01](../DESIGN.md#contract-01) — [determinism](../DESIGN.md#contract-01-4-2), [snapshot composition](../DESIGN.md#contract-01-4-3), [emission guards and error taxonomy](../DESIGN.md#contract-01-4-4); [02](../DESIGN.md#contract-02) — [binding of the line](../DESIGN.md#contract-02-4-1); [03](../DESIGN.md#contract-03) — [model formulas](../DESIGN.md#contract-03-4-1), [meter mapping](../DESIGN.md#contract-03-4-2), [windows and band continuity](../DESIGN.md#contract-03-4-3), [granularity](../DESIGN.md#contract-03-4-4); [09](../DESIGN.md#contract-09) — [proration](../DESIGN.md#contract-09-4-1), [obligation envelope](../DESIGN.md#contract-09-4-2), [split semantics](../DESIGN.md#contract-09-4-3); [10](../DESIGN.md#contract-10) — [validators](../DESIGN.md#contract-10-4-2), [ASC 606 references](../DESIGN.md#contract-10-4-4), [bundle pass-through](../DESIGN.md#contract-10-4-5). Runtime procedures of these namespaces are in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.2.
- **Dependencies**: none at feature level. Arithmetic is checked by Rating's own golden vectors against pricing's `amount_for` oracle, `pricing/tests/contract/*.json` and `pricing/tests/seam_fixtures/*.json`; the joint `bss-fixtures` corpus and its `CorpusEvaluator` were deleted (UPSTREAM_REQS H-1 superseded).
- **Consumers**: [Price Adjustments](03-price-adjustments.md) supplies the dormant slots of step 4 (contract override, commitments and reservations, coupons, FX); [Attribution and Window Counters](05-attribution-counters.md) lays out counters with `split_points`; [Commercial Facts and Window Scheduling](06-fact-scheduling.md) derives expected children with `window_geometry`; [Child Evaluation](07-child-evaluation.md) calls `evaluate`; [Pre-Purchase Order Evaluation](10-order-evaluation.md) calls `evaluate_order`.
- **Upstream**: prices as bindings of Pricing's `PricingReadV1::resolve` (T-D-73), money as `PriceModel` decimals (T-D-74), windows from `UsageRatingPolicy` (T-D-75), derived usage declarations from Products (T-D-76, R-31); `cpt-cf-bss-rating-upreq-level-meter-rule`, `cpt-cf-bss-rating-upreq-dimension-encoding`, `cpt-cf-bss-rating-upreq-fixtures-band-scale` ([UPSTREAM_REQS](../UPSTREAM_REQS.md) §2.4, §2.5, §2.14). `cpt-cf-bss-rating-upreq-pricing-validator-hook` is closed: approval is the shared `bss-approval` engine and Rating has no role (R-12).

**UI applicability**: none; the core is an in-process library with no endpoint and no user interface.

## 2. Actor Flows (CDSL)

**Use cases**: `cpt-cf-bss-rating-usecase-tariff-editor` — authoring and approval are Pricing's (the shared `bss-approval` engine); this feature only evaluates the bindings Pricing resolves and reports the rating-time checks that remain (§2.2). `cpt-cf-bss-rating-usecase-finance-simulation` is deferred (T-D-32) and has no flow here.

### 2.1 Evaluate one child input

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-core-evaluate`

**Actor**: `cpt-cf-bss-rating-actor-rating` (the rater, inside its transaction)

**Success Scenarios**: the named engine generation returns an `EvaluationOutcome` with exact lines, lineage, obligations and `input_digest`; the same input returns byte-identical output on any worker at any time.

**Error Scenarios**: the generation is not retained (`engine_unavailable`); an input defect returns a typed `EvaluationError` from the closed taxonomy (`price_uncovered`, `binding_mismatch`, `unsupported_input_fold`, `non_injective_mapping`, …); a missing derived declaration never reaches the core — the pipeline keeps the child `pending(derived_declaration_unavailable)`; a bound is exceeded (`precision_overflow`, `quantity_out_of_range`).

**Steps**:
1. [ ] - `p1` - Resolve the evaluator with `EngineRegistry::get(engine_generation)`; **IF** absent, return `engine_unavailable` and never substitute another generation - `inst-ev-registry`
2. [ ] - `p1` - Validate the input for presence and consistency (fact, `PricingInput` with its bindings and `binding_digest`, the derived declaration for a usage child, per-slice, per-granule and per-input quantities with `q_version`, layout version, group context when grouped) - `inst-ev-validate`
3. [ ] - `p1` - Run the step order in the compiled order over every slice or line of the child (§3.1) - `inst-ev-steps`
4. [ ] - `p1` - Compose the snapshot body per line and the lineage; compute `input_digest` - `inst-ev-snapshot`
5. [ ] - `p1` - **RETURN** the outcome, or the first typed error; nothing is persisted by the core - `inst-ev-return`

The rater half of this flow — locking, assembling the input and persisting the outcome — is `cpt-cf-bss-rating-seq-evaluate-tariff` in [Child Evaluation](07-child-evaluation.md).

### 2.2 Check the rules that remain at evaluation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-core-rule-verdicts`

**Actor**: `cpt-cf-bss-rating-actor-rating` (at evaluation)

**Success Scenarios**: a binding set that passes the remaining rating-time checks evaluates normally.

**Error Scenarios**: a failed check returns `non_injective_mapping` (`rating-val-02`); the child fails and no charge is produced.

**Steps**:
1. [ ] - `p1` - There is no publish-time registration: approval is the shared `bss-approval` engine, Rating is not one of its sources, and Pricing runs its own publish checks (`METER_DUPLICATE`, `ITEM_UNCOVERED`, `FREQUENCY_MIXED`; R-12 closed). The former `rating-val-01` (overlay precedence) and `rating-val-03` (overlay chain cap) have no input because `PriceOverlay` was removed from Pricing - `inst-rv-target`
2. [ ] - `p1` - At evaluation, check that no two cells of one line resolve to the same derived meter and dimension value (`rating-val-02`, defensive; it mirrors Pricing's `METER_DUPLICATE`) - `inst-rv-interim`
3. [ ] - `p1` - **RETURN** `non_injective_mapping` on a failing check; `rating-val-04` (`contract_dimension_undeclared`) is dormant until a Contracts source exists (R-11) - `inst-rv-return`

<a id="register-flows"></a>

### Migrated namespace flows

The flows of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-evaluate-line-fnd`
  — Core foundation — Interactions and Sequences ([contract](#contract-01-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-reresolve-open-window-fnd`
  — Core foundation — Interactions and Sequences ([contract](#contract-01-3-6))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-select-base-row-sel`
  — Selection — Interactions and Sequences ([contract](#contract-02-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-phase-fallback-sel`
  — Selection — Interactions and Sequences ([contract](#contract-02-3-6))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-price-line-mm`
  — Metering models — Interactions and Sequences ([contract](#contract-03-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-composite-eval-mm`
  — Metering models — Interactions and Sequences ([contract](#contract-03-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-dimension-routing-mm`
  — Metering models — Interactions and Sequences ([contract](#contract-03-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-sub-window-split-ppc`
  — Period and plan change — Interactions and Sequences ([contract](#contract-09-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-period-floor-cap-ppc`
  — Period and plan change — Interactions and Sequences ([contract](#contract-09-3-6))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-publish-gate-gov`
  — Governance — Interactions and Sequences ([contract](#contract-10-3-6))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-bundle-sum-passthrough-gov`
  — Governance — Interactions and Sequences ([contract](#contract-10-3-6))

## 3. Processes / Business Logic (CDSL)

### 3.1 Run the fixed step pipeline

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-core-step-pipeline`

**Input**: a validated `EvaluationInput`.

**Output**: per-slice `RatedLine`s, or an `EvaluationError`.

1. [ ] - `p1` - Lay out lines by child kind: per slice of `quantities.per_slice` for `usage_window`; per slice of the period for `period_line`; one line for `one_time` (only if R-19 is accepted) (DESIGN §3.1). There is no `usage_event` kind: Pricing offers no per-event window (T-D-75) - `inst-sp-layout`
2. [ ] - `p1` - Bind: take the line's `AcceptedBinding` from `PricingInput` and cross-check it against the fact's accepted price binding (§3.2) - `inst-sp-select`
3. [ ] - `p1` - Meter and model: for a usage line, evaluate the derived usage type per granule (`DerivedMeterEvaluator`, T-D-76), (no quantity round-up exists on `main`; Products' declared per-granule rounding is the only quantity rounding, DESIGN [03 §4.4](../DESIGN.md#contract-03-4-4)), then apply the binding's `PriceModel` formula with the band offset carried across the child's slices (§3.3) - `inst-sp-model`
4. [ ] - `p1` - Dormant steps: contract overlays, commitments, coupons and FX have no source and fail closed if an input names them ([Price Adjustments](03-price-adjustments.md)) - `inst-sp-adjust`
5. [ ] - `p1` - Guards: emission guards in order, then lineage and snapshot body ([01 §4.4](../DESIGN.md#contract-01-4-4)); the minimum-fee floor is applied at parent roll-up, not here (T-D-78) - `inst-sp-guards`
6. [ ] - `p1` - **RETURN** the lines; no configuration reorders any step - `inst-sp-return`

### 3.2 Take the bound price and check the binding

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-core-selection`

**Input**: the line's item, dimension value and slice; `PricingInput {plan_id, revision_id, resolve_date, bindings, binding_digest}`; the fact's accepted price binding (its pins).

**Output**: exactly one `AcceptedBinding` for the line, or an error.

1. [ ] - `p1` - Find the cell for `(item_id, dimension_value)` in the bindings that cover the slice; a slice after a binding's `ends_on` uses the binding of the second resolve at that date (pricing D-425) - `inst-sel-phase`
2. [ ] - `p1` - **IF** the cell has no binding (pricing answered *uncovered*), return `price_uncovered`; uncovered is never free (pricing D-420) - `inst-sel-candidates`
3. [ ] - `p1` - **IF** the binding's `price.state` is not `Approved`, return `price_uncovered` - `inst-sel-assert`
4. [ ] - `p1` - **IF** the bound `price_id` differs from the price the fact's accepted binding names for that item and value, return `binding_mismatch` - `inst-sel-binding`
5. [ ] - `p1` - **RETURN** the binding with `price_id`, `price_book_entry_id`, `money_digest`, `sku_id`, `sku_version` and `binding_digest` in lineage - `inst-sel-return`

Rating selects nothing: there is no scope key, cohort, phase, eligibility class, price window or overlay to match. Pricing's `resolve` binds the price in force for a signup and walks `all` successors for a renewal, stopping before the first `new` one (pricing D-420); `eligibility: all|new` is an input to that walk, not to Rating (T-D-73).

### 3.3 Price a line by its model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-core-model`

**Input**: the binding (its `PriceModel` and `usage_rating_policy`), the line's slice quantities (per granule and input for a usage line), the derived declaration, the running band offset of the child (and the group context's prefix when grouped).

**Output**: a model line outcome per slice.

1. [ ] - `p1` - **IF** the derived declaration has a `Peak` or `TimeWeighted` input, return `unsupported_input_fold` (R-06) - `inst-mod-sum`
2. [ ] - `p1` - **IF** the binding carries a non-empty `dimension_key` whose value cannot be mapped from the record metadata, return `unsupported_primitive` until R-16 is decided - `inst-mod-dimension`
3. [ ] - `p1` - **FOR EACH** slice in order: the slice quantity is the sum over its granules of `bss_products_sdk::derived::evaluate` on the granule's input quantities (`fold = SUM`, T-D-75, T-D-76); place it on the bands at the running offset and apply the formula of [03 §4.1](../DESIGN.md#contract-03-4-1): `flat` → `amount`; `per_unit` → `unit_amount × q`; `graduated` → sum over bands; `volume` → `q × rate` of the first band with `q < up_to`; `package` → `ceil(q / package_size) × package_price` - `inst-mod-slices`
4. [ ] - `p1` - Keep each amount in major units as an exact fraction tagged with the binding's `currency` and `InvoiceInputs.currency_scale`; nothing is scaled by `10^currency_scale` or rounded (T-D-74, T-D-80) - `inst-mod-composite`
5. [ ] - `p1` - **RETURN** exact amounts; tiers restart only at `rating_window_start`, a clipped window rates its actual quantity against whole thresholds (`actual_quantity_full_thresholds`), and bands never reset inside a child ([03 §4.3](../DESIGN.md#contract-03-4-3)) - `inst-mod-return`

### 3.4 Compute split points, window geometry, proration and the minimum-fee floor

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-core-geometry`

**Input**: the binding's `usage_rating_policy`, the fact's billing period and served extent, the bindings' `ends_on`, the subscription's committed quantity intervals.

**Output**: `split_points`, the expected `RatingWindow`s of a fact, prorated period lines, the minimum-fee floor of a price.

1. [ ] - `p1` - `window_geometry` returns the windows of `rating_window`: `BillingCycle` → the fact's billing period; `CalendarHour{Utc}` → each UTC hour of the served extent; a partial window keeps its canonical bounds (T-D-75) - `inst-geo-windows`
2. [ ] - `p1` - `split_points` returns only a binding's `ends_on` inside the child (never `effective_to`, pricing D-425), a term-slice change and, for a `period_line`, a quantity change carried by the fact; UTC hours are granules or windows and a billing-period boundary or a plan change separates children, so none of them is a split point; split points add slices, never children ([09 §4.3](../DESIGN.md#contract-09-4-3)) - `inst-geo-splits`
3. [ ] - `p1` - Prorate each recurring `period_line` slice by the covered fraction = covered UTC seconds / the billing period's UTC seconds, exact (T-D-77; R-30) - `inst-geo-prorate`
4. [ ] - `p2` - Provide the floor function used at roll-up: billed = `max(Σ exact amounts rated by one price_id, minimum_fee × covered_fraction)`, only for `BillingCycle` + `subscription_line` entries (T-D-78) - `inst-geo-obligation`
5. [ ] - `p1` - **RETURN** values only; the functions are pure and shared by the pipeline and `evaluate` - `inst-geo-return`

### 3.5 Exact arithmetic under a budget

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-core-exact-arithmetic`

**Input**: operands as `ExactQuantity` / `ExactAmount` and the generation's `ArithmeticBudget`.

**Output**: reduced exact results, or a deterministic bound error.

1. [ ] - `p1` - Convert pricing decimals exactly into major-unit fractions with the binding's `currency` and `currency_scale`; combine two amounts only when both match, else `binding_currency_mismatch`; never use floating point, and never use `rust_decimal` arithmetic for a result that is not proven to fit (its checked operations round past 28 fractional digits) - `inst-ex-convert`
2. [ ] - `p1` - Check the operand bit budget before each multiply and divide; **IF** exceeded, return `precision_overflow` - `inst-ex-budget`
3. [ ] - `p1` - Reduce every amount; **IF** a result exceeds the wire and storage bounds of DESIGN §3.3, return `precision_overflow` or `quantity_out_of_range` before anything can be persisted - `inst-ex-bounds`
4. [ ] - `p1` - **RETURN** without rounding money; the only rounding is declared business rounding of quantities (Products' per-granule rounding, the package `ceil`); Billing rounds each invoice line once at the stored `currency_scale` (T-D-51, T-D-80) - `inst-ex-return`

### 3.6 Release and retain engine generations

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-core-engine-generations`

**Input**: a change of evaluation semantics.

**Output**: a new frozen module `engine::gen_<n>` with its `engine_digest`.

1. [ ] - `p1` - Put every change of formulas, step order, exact-arithmetic rules, budget or error classification into a new generation; a released generation's module is frozen - `inst-eg-new`
2. [ ] - `p1` - Compute `engine_digest` over the generation's frozen source tree and golden-vector outputs at build time; the releasing migration records it in `bss_rating__engine_generation` - `inst-eg-digest`
3. [ ] - `p1` - Keep every generation compiled in while any retained result references it; removal needs an ADR and proof of no reference (DESIGN §4.1) - `inst-eg-retain`
4. [ ] - `p1` - Run Rating's own golden vectors per retained generation, cross-checked against pricing's `amount_for` oracle (`pricing/src/domain/money.rs`) and pricing's seam fixtures; the joint `bss-fixtures` corpus and its `CorpusEvaluator` were deleted with pricing phase 4 (DESIGN §1.3) - `inst-eg-corpus`

### 3.7 Pass through governance evidence

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-algo-core-governance-passthrough`

**Input**: the bindings of a line.

**Output**: lines carrying ASC 606 references and invoice inputs.

1. [ ] - `p2` - Set `performance_obligation_ref` and `ssp_snapshot_pointer` to null: no PriceBook binding carries them ([10 §4.4](../DESIGN.md#contract-10-4-4)); copy the binding's `InvoiceInputs` (`gl_code`, `tax_category`, `template`) onto the line - `inst-gov-asc606`
2. [ ] - `p1` - Bundle summing is dormant: Pricing refuses a bundle SKU (`BUNDLE_SKU_NOT_PRICEABLE`) and the sold-as bundle is deferred (pricing D-411); a line that names a bundle fails closed ([10 §4.5](../DESIGN.md#contract-10-4-5)) - `inst-gov-bundle`

### 3.8 Evaluate an order input

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-algo-core-evaluate-order`

**Input**: an `OrderEvaluationInput` built from `OrderEvaluationRequest` (Orders D-167): per line the `plan_revision_id`, per item the `quantity` and the exact chain bindings; no subscription version and no renewal walk (T-D-79).

**Output**: an `OrderEvaluation` or an `EvaluationError`.

1. [ ] - `p2` - Run the same model formulas, money conversion, proration and minimum-fee floor under the current generation; usage is flagged and excluded from totals (T-D-54, T-D-79) - `inst-ord-steps`
2. [ ] - `p2` - **RETURN** the separate order DTO; nothing is persisted and no idempotency key is involved - `inst-ord-return`

The SDK surface, authorization and price reads are [Pre-Purchase Order Evaluation](10-order-evaluation.md).

## 4. States (CDSL)

### 4.1 Engine generation lifecycle

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-core-engine-generation`

**States**: `current` (evaluates new work), `retained` (frozen; evaluates corrections of children whose engine-of-record it is, re-rates targeting it, and replays), `removable` (no retained result references it).

**Initial State**: `current`, when the releasing migration records the generation and its `engine_digest`.

**Transitions**:
1. [ ] - `p1` - **FROM** `current` **TO** `retained` **WHEN** a later generation is released; the module is frozen and stays compiled in - `inst-egs-retain`
2. [ ] - `p2` - **FROM** `retained` **TO** `removable` **WHEN** no retained result references the generation, proven after re-rating those results, and an ADR approves the removal (DESIGN §4.1) - `inst-egs-removable`

A generation is never replaced in place: a semantic change is always a new generation, and a missing one is `engine_unavailable`.

## 5. Definitions of Done

### 5.1 Pure evaluation entry points

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-core-evaluate`

The system **MUST** provide `cf-gears-bss-rating-core` with `EngineRegistry`, `evaluate`, `evaluate_order`, `split_points`, `window_geometry` and `DerivedMeterEvaluator` per DESIGN §3.3, run the step order (bind, meter, model, guards; floor at roll-up) in the compiled order, apply the emission guards and return only typed errors of the closed taxonomy, with no I/O, no clock read and no store; a CI deny-list rejects tokio, sea-orm, http and ClientHub in the crate.

**Implements**: `cpt-cf-bss-rating-flow-core-evaluate`, `cpt-cf-bss-rating-algo-core-step-pipeline`, `cpt-cf-bss-rating-algo-core-evaluate-order`, `cpt-cf-bss-rating-flow-evaluate-line-fnd`.

**Constraints**: `cpt-cf-bss-rating-constraint-stateless-hot-path`, `cpt-cf-bss-rating-constraint-no-store-fnd`, `cpt-cf-bss-rating-constraint-utc-money-fnd`.

**Touches**: `cpt-cf-bss-rating-interface-core-evaluate`, `cpt-cf-bss-rating-interface-evaluate-fnd`; entities `EvaluationInput`, `EvaluationOutcome`, `RatedLine`.

### 5.2 Bindings, models, windows and period math

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-core-pricing-semantics`

The system **MUST** take each line's price from the resolved bindings and cross-check it against the fact's accepted binding (T-D-73; no selection is built), convert `PriceModel` decimals exactly into major-unit fractions carrying `currency` and `currency_scale` (T-D-74, T-D-80), apply the model formulas, evaluate derived usage types per granule (T-D-76), derive windows from `UsageRatingPolicy` (T-D-75), compute split points at `ends_on` with band continuity, prorate by covered UTC seconds (T-D-77) and provide the minimum-fee floor (T-D-78) exactly as namespaces 02, 03 and 09 define them, and fail closed with the launch errors for `Peak` / `TimeWeighted` inputs, non-empty dimension keys and other unsupported primitives.

**Implements**: `cpt-cf-bss-rating-algo-core-selection`, `cpt-cf-bss-rating-algo-core-model`, `cpt-cf-bss-rating-algo-core-geometry`, `cpt-cf-bss-rating-flow-select-base-row-sel`, `cpt-cf-bss-rating-flow-phase-fallback-sel`, `cpt-cf-bss-rating-flow-price-line-mm`, `cpt-cf-bss-rating-flow-composite-eval-mm`, `cpt-cf-bss-rating-flow-dimension-routing-mm`, `cpt-cf-bss-rating-flow-sub-window-split-ppc`, `cpt-cf-bss-rating-flow-period-floor-cap-ppc`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-silent-fallback-sel`, `cpt-cf-bss-rating-constraint-phase-id-only-sel`, `cpt-cf-bss-rating-constraint-typed-quantity-mm`, `cpt-cf-bss-rating-constraint-no-round-no-execute-ppc`, `cpt-cf-bss-rating-constraint-utc-half-open-ppc`.

**Touches**: `cpt-cf-bss-rating-interface-select-base-row-sel`, `cpt-cf-bss-rating-interface-price-line-mm`, `cpt-cf-bss-rating-interface-split-evaluation-ppc`, `cpt-cf-bss-rating-interface-period-obligation-ppc`.

### 5.3 Exact numbers and retained engine generations

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-core-exact-and-replay`

The system **MUST** compute every amount and quantity exactly under the generation's arithmetic budget, fail deterministically beyond the bounds of DESIGN §3.3, never round, release each semantic change as a new frozen generation with a recorded `engine_digest`, and run every retained generation's golden corpus plus a cross-upgrade replay test in CI.

**Implements**: `cpt-cf-bss-rating-algo-core-exact-arithmetic`, `cpt-cf-bss-rating-algo-core-engine-generations`, `cpt-cf-bss-rating-state-core-engine-generation`, `cpt-cf-bss-rating-flow-reresolve-open-window-fnd`.

**Constraints**: `cpt-cf-bss-rating-constraint-exact-no-rounding`, `cpt-cf-bss-rating-constraint-utc-money-fnd`.

**Touches**: `cpt-cf-bss-rating-dbtable-engine-generation` (rows written by the releasing migration; start-up verification is [Child Evaluation](07-child-evaluation.md)); Rating's golden vectors; pricing's golden contract and seam fixtures.

### 5.4 Governance pass-through and defensive rules

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-core-governance`

The system **MUST** run the remaining rating-time check (`rating-val-02`) defensively (`non_injective_mapping`), register nothing in Pricing's publish pipeline (R-12 closed: approval is the shared `bss-approval` engine), carry the binding's invoice inputs onto every line, keep ASC 606 references null and fail closed on a bundle line (pricing D-411).

**Implements**: `cpt-cf-bss-rating-flow-core-rule-verdicts`, `cpt-cf-bss-rating-algo-core-governance-passthrough`, `cpt-cf-bss-rating-flow-publish-gate-gov`, `cpt-cf-bss-rating-flow-bundle-sum-passthrough-gov`.

**Constraints**: `cpt-cf-bss-rating-constraint-no-second-workflow-gov`, `cpt-cf-bss-rating-constraint-publish-time-gov`, `cpt-cf-bss-rating-constraint-immutable-refs-gov`.

**Touches**: `cpt-cf-bss-rating-interface-validator-registration-gov`, `cpt-cf-bss-rating-interface-asc606-envelope-gov`.

## 6. Acceptance Criteria

- [ ] Rating's golden vectors pass for every retained generation and agree with pricing's `amount_for` oracle (the joint `bss-fixtures` corpus no longer exists; UPSTREAM_REQS H-1, H-2 superseded); pricing's golden contract and seam fixtures (`pricing/tests/contract/*.json`, `pricing/tests/seam_fixtures/*.json`) are the binding inputs, and cloudlets Q = 8 and Q = 12 rate 0.34 hourly (volume) and graduated 12 rates 0.23.
- [ ] The Atlas arithmetic fixtures F02, F03, F10, F19, F23–F29 and F32 produce the exact amounts DESIGN §4.3 and namespaces 03 and 09 state; no intermediate value is rounded.
- [ ] Evaluating the same input twice, on different workers or after a restart, returns byte-identical outcomes and the same `input_digest`.
- [ ] Hourly bands reset per window, a partial window keeps its thresholds, and band continuity holds across the slices of one child (DESIGN §4.3 worked example A).
- [ ] **V28** A derived usage type with `Sum` inputs evaluated per UTC hour and summed per window equals `bss_products_sdk::derived::evaluate_window` over the same granules wherever `evaluate_window`'s `rust_decimal` sum is exact; Rating's window sum is an exact `ExactQuantity` (T-D-76, T-D-80).
- [ ] **V28 (fold gate)** A derived usage type with a `Peak` or `TimeWeighted` input fails `unsupported_input_fold`; an unmappable `dimension_key` fails `unsupported_primitive`; a cell without a binding fails `price_uncovered`.
- [ ] A line amount below zero after step 3 (or after any dormant slot of step 4) is clamped to zero with the pre-clamp amount in lineage; a negative window quantity fails `negative_window_quantity`.
- [ ] Rating never selects or falls back to another price: an uncovered cell and a binding that differs from the fact's accepted binding fail as DESIGN §4.13 V33 states (owned here; T-D-73).
- [ ] **V30** Pricing money stays exact in major units: `PerUnit{unit_amount: "0.015"}` × 12 in EUR (`currency_scale = 2`) is exactly `9/50` EUR (0.18); a rate of `0.005` EUR stays `1/200`; JPY and KWD carry scale 0 and 3; no minor-unit or nano-minor scale exists (T-D-74, T-D-80).
- [ ] **V29** A mid-period plan change prorates both `period_line` children by covered UTC seconds and their fractions sum to 1 (T-D-77; 09 §3.6 worked example in §7).
- [ ] The floor function returns the amounts DESIGN §4.13 V34 states (vector owned by [Roll-up and Delivery](09-rollup-delivery.md); T-D-78).
- [ ] An intermediate operand over the arithmetic budget fails `precision_overflow` before any result is returned.
- [ ] Rating's own arithmetic agrees with pricing's `amount_for` on every case whose exact result fits `rust_decimal`, and stays exact where `amount_for` would round (V39).
- [ ] Rating serializes every `ExactAmount` and `ExactQuantity` as canonical strings with `currency` and `currency_scale` beside each amount, and rejects a JSON number in those fields with `InvalidArgument` (T-D-80; V41, V47).
- [ ] A binding set that violates `rating-val-02` fails `non_injective_mapping` at evaluation; no line is emitted.
- [ ] `evaluate_order` excludes usage from totals and returns the same exact arithmetic as `evaluate` for the same recurring binding.
- [ ] The crate builds with the dependency deny-list enforced.

Acceptance vectors owned by this feature (DESIGN §4.13 index):

| # | Scenario | Expected state |
|---|---|---|
| V18 | A binding's `ends_on` falls inside a window (a temporary price ends) | slices after `ends_on` use the binding of the second resolve at that date; no new child, no tier reset (pricing D-425) |
| V22 | A result of generation 1 replayed in a build whose current generation is 2 | identical outcome; a missing generation fails `engine_unavailable` |
| V26 | A dimension value with its own chain and one bound through the default chain (`via_default`) — R-16-gated: launch rates the empty dimension only | each line takes its own cell; no ambiguity; Rating selects nothing |
| V35 | `PerUnit{unit_amount: "0.047"}` EUR × 2.5; a rate of `0.004` × 1 (rate below the posting increment) | **Rating**: exact `47/400` and `1/250`, unrounded, scale 2. Billing golden (R-32): 0.12 and 0.00 |
| V38 | Recurring `Flat{amount: "1.00"}` per month, 30-day period, 864 000 of 2 592 000 s covered | **Rating**: exactly `1/3`, delivered as a fraction; three such thirds on one line sum to `1/1`. Billing golden: 0.33; the three-thirds line 1.00, never 0.99 |
| V39 | Rate `1e-28` × quantity `1e-28` (written out); `unit_amount` 10^27 × `Q = 10^57 − 1` and × `Q = 10^57` | **Rating**: exactly `1/10^56` (`rust_decimal` would return 0); the first product is exact; the second reaches `10^84` and fails `precision_overflow`, nothing persisted |
| V40 | The exact amount `1/8` in EUR (scale 2) and KWD (scale 3); `1/2` and `3/2` in JPY (scale 0) | **Rating**: the same fraction, tagged with each binding's scale; nothing multiplied by `10^currency_scale`. Billing golden: 0.12, 0.125, 0 and 2 |
| V41 | EUR and USD bindings in one child; EUR amounts at scale 2 and 3 in one sum; a JSON number in a Rating DTO numeric field | **Rating**: `binding_currency_mismatch` (never converted); `InvalidArgument`. An EUR binding at scale 3 is refused by Pricing, not reached by Rating |
| V42 | `Package{package_size: "10", package_price: "1.00"}` at `q = 10` and `q = 10.0000001`; volume `up_to: 10` at `q = 10`; a derived type with `output_scale = 2` | **Rating**: 1 and 2 blocks; the volume band selected at `q = 10` is the upper one (graduated bills all ten units in `[0, 10)`); Products' per-granule rounding applied before Rating sums granules; no amount rounded. `per_hour` minimum billable units: dormant until Pricing publishes the field (R-33) |
| V47 | Stored quantities `10.000` / `10`, prices `0.0470` / `0.047`; a computed `2/4`; zero; the texts `1e1`, `+1`, `01`, `-0` | **Rating**: one canonical text, `input_digest` and `rsnap1` per pair; `1/2`; `0/1`; the four texts rejected |

## 7. Detailed Behavior Contracts

**Contract namespaces 01, 02, 03, 09, 10.** Section numbers inside the detailed contracts below resolve through the [contract address index](../DECOMPOSITION.md#4-contract-address-index), not the overview section numbers above. A bare section reference names a section of the same namespace; "DESIGN §n" names §1–§5 of [DESIGN.md](../DESIGN.md).

The following sections keep the runtime procedures of these namespaces — their interactions and sequences and the procedural parts of their §4. The flows, processes and acceptance criteria above summarize them; the architecture, schemas and evaluation semantics they rely on are defined once in [DESIGN.md](../DESIGN.md) §6.

<a id="contract-01-3-6"></a>

<!-- contract:01-foundation:3.6 -->
### Core foundation: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-evaluate-line-fnd` (`p1`), defined in [§2 migrated flows](#register-flows).

**Evaluate a child** (the core half of `cpt-cf-bss-rating-seq-evaluate-tariff`):

1. `InputValidator` checks presence and consistency; failure returns an error.
2. For `usage_window`: take slices from `quantities.per_slice` (already laid out by
   the line layout, per part and input, folded per UTC-hour granule); for `period_line`: one line per slice of the period
   (quantity changes); for `one_time`: one line.
3. Bind ([contract 02](../DESIGN.md#contract-02)): the line's `AcceptedBinding` from `PricingInput`, cross-checked against the
   fact's accepted binding (T-D-73).
4. Meter and model ([contract 03](../DESIGN.md#contract-03)): derived evaluation per granule (T-D-76) — no granularity round-up exists on `main` —,
   the binding's `PriceModel` formula with the in-window band offset carried across slices, exact
   amounts kept as exact major-unit fractions (T-D-74, T-D-80).
5. Dormant steps ([contract 04](../DESIGN.md#contract-04), [05](../DESIGN.md#contract-05), [06](../DESIGN.md#contract-06), [07](../DESIGN.md#contract-07)): no source; fail closed if named.
6. Guards: `EmissionGuard`, `SnapshotComposer`, lineage; compute `input_digest`.

**Contract**: `cpt-cf-bss-rating-flow-reresolve-open-window-fnd` (`p2`), defined in [§2 migrated flows](#register-flows).

**Re-evaluation after late usage** (core half of `cpt-cf-bss-rating-seq-correction`): the child's
counter gains `q_version + 1`; the pipeline rebuilds the input with the child's bindings
(provisional: a resolve with the fact's current pins at the period start; final: the binding of
record, T-D-42, T-D-73 — no new `resolve`) and the new quantities; `evaluate` returns the full
new outcome; the pipeline records a new revision. Whether Billing treats the next parent revision as
a draft replacement or a credit/debit note is Billing's decision (T-D-50), never the core's.

Example: a `volume` window at 70 units rated 70.00 EUR; a late 50-unit record arrives; the new
evaluation over 120 units yields 96.00 EUR (exactly `96/1` EUR at scale 2); the next parent revision carries
96.00 and Billing derives +26.00 (rounded corrected total 96.00 minus the cumulative posted 70.00).

<!-- /contract -->

<a id="contract-02-3-6"></a>

<!-- contract:02-selection-eligibility:3.6 -->
### Selection: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-select-base-row-sel` (`p1`), defined in [§2 migrated flows](#register-flows).

**Take the bound price** (the bind step of `cpt-cf-bss-rating-seq-evaluate-tariff`): find the cell
for the line's item and dimension value → **uncovered** ⇒ `price_uncovered` → compare the bound
`price_id` with the fact's accepted binding ⇒ `binding_mismatch` → emit the binding into lineage. A recurring and
a usage item of one plan revision are separate cells, each with its own binding (pricing entry key
`(book_id, sku_id, charge_kind, period, model, usage_policy_digest)`, D-502).

**Contract**: `cpt-cf-bss-rating-flow-phase-fallback-sel` (`p2`), defined in [§2 migrated flows](#register-flows).

**Superseded** (T-D-73): Pricing has no phases (spec item 1, trials dropped), so there is no
phase-invariant fallback. A dimension value without its own price in force is bound through the
default chain by Pricing itself (`via_default = true`, pricing D-420); Rating records the flag in
lineage and does nothing else.

<!-- /contract -->

<a id="contract-03-3-6"></a>

<!-- contract:03-metering-models:3.6 -->
### Metering models: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-price-line-mm` (`p1`), defined in [§2 migrated flows](#register-flows).

**Price one unit** (the meter and model steps of `cpt-cf-bss-rating-seq-evaluate-tariff`): take the
binding's `PriceModel` → evaluate the derived meter per granule and sum into the slice → for each
slice in order: place on the binding's bands at the running offset (no quantity round-up exists on
`main`), compute the exact major-unit amount with the binding's `currency` and `currency_scale` →
emit one `ModelLineOutcome` per slice.

**Contract**: `cpt-cf-bss-rating-flow-composite-eval-mm` (`p2`), defined in [§2 migrated flows](#register-flows).

**Derived usage meter** (T-D-39, T-D-76): every usage SKU sells a Products derived usage type
`products.derived/<code>@<n>` (products P-D-259); the binding's `meter` is
`MeterRef{usage_type_id, version}`. The declaration — inputs (raw collector GTS usage types, at
least one, `MIN_INPUTS = 1`), each with a `GranuleFold`, the formula as data, granularity `Hour`,
`output_unit`, `output_scale`, `output_round` — is read once and frozen with the result
(`bss_rating__derived_declaration`; the typed read is R-31). Counters are kept per raw input and UTC
hour ([contract 13](../DESIGN.md#contract-13)). Per granule the core folds each input, calls
`bss_products_sdk::derived::evaluate`, and the slice quantity is the sum of its granule outputs
(`evaluate_window`). A one-input `Sum` wrapper of a raw meter (P-D-251) reduces to the raw sum. At
launch only `Sum` inputs are rateable (R-06); a `Peak` or `TimeWeighted` input fails closed
`unsupported_input_fold`. A derived type whose inputs carry non-empty dimension values is
`unsupported_primitive` until the input-join rule is decided — a Rating item [OPEN QUESTION —
derived meter × dimensions, DECISIONS carried opens].

**Contract**: `cpt-cf-bss-rating-flow-dimension-routing-mm` (`p2`), defined in [§2 migrated flows](#register-flows).

**Dimension routing**: see §4.2 launch posture.

<!-- /contract -->

<a id="contract-09-3-6"></a>

<!-- contract:09-period-plan-change:3.6 -->
### Period and plan change: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-sub-window-split-ppc` (`p2`), defined in [§2 migrated flows](#register-flows).

**Mid-period plan change** (recurring line, September = 30 days = 2 592 000 s, plan A €30.00/month,
plan B €60.00/month; proration by covered UTC seconds, T-D-77):

1. Subscriptions records a plan change effective `2026-09-11T00:00Z`; two recurring facts exist for September: `lineKey = plan#1` and `lineKey = plan#2`, each with its own pins.
2. Each fact creates a `period_line` child ([contract 14](../DESIGN.md#contract-14)); each child prorates over its interval.
3. `plan#1` covers Sep 1–10 → 864 000 / 2 592 000 × 30.00 = **10.00**; `plan#2` covers Sep 11–30 → 1 728 000 / 2 592 000 × 60.00 = **40.00**; total 50.00.
4. If the change were at `2026-09-11T14:00Z`, the fractions are 914 400 / 2 592 000 and 1 677 600 / 2 592 000: `plan#1` = **10.5833…** (exact `127/12` EUR), `plan#2` = **38.8333…** (exact `233/6` EUR); the fractions still sum to 1 and Billing rounds once.
5. Usage windows of the subscription are split at the same instant. A counter continues across the change when the continuation key matches (subscription, priced SKU/meter/dimension, compatible window identity, T-D-36); otherwise it starts at zero. Subscriptions schedules an incompatible usage-policy change at the next UTC hour boundary (pricing D-510 E4); a `CalendarHour` change that is not on an hour boundary fails the child closed (`intra_window_policy_change`).

**Contract**: `cpt-cf-bss-rating-flow-period-floor-cap-ppc` (`p2`), defined in [§2 migrated flows](#register-flows).

**Minimum-fee floor** (T-D-38, T-D-78):

1. A usage binding of a `BillingCycle` + `subscription_line` entry carries `minimum_fee` (a `CalendarHour` or `resource`-scoped entry never does, pricing D-503, D-504).
2. At parent roll-up, per `price_id` in the fact, the floor function sums the exact amounts of every slice and dimension value rated by that price and compares the sum with `minimum_fee × covered_fraction`.
3. If the floor is higher, the difference is a separate exact `min_fee_topup` line with the price's lineage; later revisions recompute it deterministically. Plan-level floors and caps (`PeriodFloorCap`) no longer exist in Pricing (D-467); Billing rounds the result.

<!-- /contract -->

<a id="contract-10-3-6"></a>

<!-- contract:10-governance-asc606:3.6 -->
### Governance: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-publish-gate-gov` (`p1`), defined in [§2 migrated flows](#register-flows).

**Rule enforcement**:

1. *Publish*: Pricing approves prices and plan revisions through the shared `bss-approval` engine and runs its own checks (`METER_DUPLICATE`, `ITEM_UNCOVERED`, `FREQUENCY_MIXED`); Rating registers nothing (R-12 closed).
2. *Evaluation*: `evaluate` runs the remaining defensive check (`rating-val-02`) over the bindings it uses; a failure returns `non_injective_mapping`, the child is `failed`, and a `bss_rating__exception` is raised — no charge is produced.

**Contract**: `cpt-cf-bss-rating-flow-bundle-sum-passthrough-gov` (`p1`), defined in [§2 migrated flows](#register-flows).

**Bundle summing** (dormant): Pricing refuses to price a bundle SKU (`BUNDLE_SKU_NOT_PRICEABLE`)
and the sold-as bundle is deferred (pricing D-411), so no bundle line reaches Rating; a line that
names one fails closed.

<!-- /contract -->
