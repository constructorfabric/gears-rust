<!-- CONFLUENCE_TITLE: [BSS]: Rating — Design Set -->
<!-- Related: ../DESIGN.md, ../PRD.md, ../SEAMS.md, ../ADR/ | Owners: BSS Rating team -->

# Rating — Design Set

<!-- toc -->

- [Slice documents](#slice-documents)
- [Slice map (PRD §6 / §17.1 step ↔ slice)](#slice-map-prd-6--171-step--slice)

<!-- /toc -->

This folder holds the **rating gear's** technical design as a **set of slice designs**
(ADR-0002): the **evaluation core** — a shared **Evaluation Foundation**
([`01-foundation.md`](./01-foundation.md)): the pure-function core, the adopted pricing 8-axis
scope key, determinism, and `pricingSnapshotRef` composition — plus per-step / per-capability
evaluator slices decomposed along the deterministic rule order (§17.1 steps 1-9), and the
**operational pipeline** (slices 12–16: ingestion, the windowed `Q` store, unit synthesis and the
period tick, rated-output persistence, the Billing handoff). Every evaluation slice runs
**through** the Foundation core over a **frozen** snapshot; none re-queries mutable catalog state
on the hot path.

The gear is not an authoring System of Record: the pricing gear owns the catalog (scope key,
`PriceWindow`, `PriceOverlay`, `CatalogVersion`, publish governance) — tariffs in the rate-card
sense live there. The cross-gear contract is frozen in [`../SEAMS.md`](../SEAMS.md); this design
implements the rating side of every resolved seam (terminology bridge for historical
"Tariffs"/"Rating" wording — the pre-consolidation names of the core and the pipeline:
[`../DESIGN.md`](../DESIGN.md)).

**The canonical index — architecture overview, slice map, dependency order, cross-cutting
normatives, the ADR index, and traceability — is [`../DESIGN.md`](../DESIGN.md).** Requirements
(WHAT/WHY) live in [`../PRD.md`](../PRD.md); decision rationale in [`../ADR/`](../ADR/) and the
cross-gear seam register in [`../SEAMS.md`](../SEAMS.md).

## Slice documents

- [`01-foundation.md`](./01-foundation.md) — **Evaluation Foundation** — The shared pure-function evaluation core that every step slice runs *through*: the evaluation context, the **adopted** pricing 8-axis canonical scope key, byte-for-byte determinism over frozen inputs, `pricingSnapshotRef` composition (Rating = composition SoR), the single-outcome / idempotency / non-negative guards, and rating-core-crate deployment (ADR-0002). (PRD §6.1, §17.1 core)
- [`02-selection-eligibility.md`](./02-selection-eligibility.md) — **Base Selection & Eligibility** — Steps 1-2: resolve the active plan phase (`phase_id`) and select the single `Price`/`PriceWindow` on the 8-axis key, applying the `priceEligibility` class order and **multi-generation `cohort` selection** by the subscription's pinned price id, with no silent fallback and the phase-invariant usage fallback. (PRD §6.3 (steps 1-2), §6.5)
- [`03-metering-models.md`](./03-metering-models.md) — **Metering & Pricing Models** — Step 3 and the model formulas: meter mapping + billing granularity; the `modelKind` set `{flat, per_unit, graduated, volume, package}` plus hybrid composition; **level-based (gauge) aggregation via the granule fold** (`aggregationFunction ∈ {peak, time_weighted}` + `aggregationGranularity`, T-D-17 / pricing D-44 — a `p1` launch capability); the tier aggregation window over the `(subscription, meter, dimensionKey, window)` counter; dimensional `(meter, dimensionKey)` lines; and composite / derived-meter evaluation. (PRD §6.2, §6.5, §6.7)
- [`04-overlays-precedence.md`](./04-overlays-precedence.md) — **Overlays & Precedence** — Steps 4-5: PriceOverlay scope resolution and the **stacking** model (all survivors applied; the class-specificity order breaks ties, it does not pick a single winner), the customer/contract overlay precedence (Contract > Partner PriceOverlay > Catalog base), and the bounded-composition anti-drift cap. (PRD §6.4 (steps 4-5))
- [`05-commitments-reservations.md`](./05-commitments-reservations.md) — **Commitments & Reservations** — Step 6: commitment-pool waterfall drawdown / overage — with the frozen `overageRate` selector deciding flat-rate vs banded residual pricing (T-D-19) — and the structured `TrueUpObligation`; reservation consumption-flavor (matched usage at the reserved rate) and capacity-flavor (`capacityCharge` on allocated quantity), with the reserved-rate sourcing split (self-service from snapshot, negotiated from Contracts). (PRD §6.6 (step 6))
- [`06-coupons.md`](./06-coupons.md) — **Coupons** — Step 7: coupon application order relative to overlays / commitment / FX, the `exclusive_best` vs `ordered_stack` stacking policies incl. cross-scope exclusivity (T-D-20), per-`applyScope` attachment, and fail-closed on missing coupon policy — including `applyScope = line_total`, which **fails closed at launch** for want of a plan-scoped evaluation base (T-D-22). (PRD §6.8 (step 7))
- [`07-currency-fx.md`](./07-currency-fx.md) — **Multi-Currency & FX** — Step 8: price / billing / presentment currency separation, the per-window rate-lock and invoice-period FX policies (provisional amount + delta at close), and `fxTableVersion` recording. (PRD §6.9 (step 8))
- [`08-retroactivity-corrections.md`](./08-retroactivity-corrections.md) — **Retroactivity & Corrections** — Posted-period immutability (delta-only corrections), open-period late-arrival re-resolution **replayed strictly from the pinned snapshot** (no live catalog read) with the administrative re-rate as the one sanctioned snapshot substitution (T-D-21), and deterministic reversal of correcting / negative usage (commitment-pool refill, tier-counter decrement) without driving a line negative. (PRD §6.10)
- [`09-period-plan-change.md`](./09-period-plan-change.md) — **Period & Plan-Change Obligations** — The period-level phases outside the per-line order: the `PeriodFloorCapObligation` (Billing-executed), mid-cycle proration on window activation, and the plan-change proration split — honoring the canonical `prorationBasis` and `billingAnchorPolicy` + D-20 clamp; Rating consumes, never decides, the change mode. (PRD §6.11)
- [`10-governance-asc606.md`](./10-governance-asc606.md) — **Governance & ASC 606** — Publish governance: Rating **registers its four fail-closed validators into the single pricing Slice 5 approval engine** (not a second workflow); ASC 606 traceable `performanceObligationRef` / `sspSnapshotPointer` (null at MVP); and the effective rev-share pass-through for bundle `sum_of_parts` (reads `effective_share_bp` only). (PRD §6.12)
- [`11-consumer-contracts.md`](./11-consumer-contracts.md) — **Consumer & Integration Contracts** — The integration surface: the Rating handoff (resolved outcome + `pricingSnapshotRef` + obligations), the Pricing read-model input contract (enums adopted verbatim; bundle summing; snapshot replay), the Subscriptions input contract, and the Finance-FX / Promotions-coupon / Billing-`periodState` contracts. (PRD §9)
- [`12-usage-ingestion-normalization.md`](./12-usage-ingestion-normalization.md) — **Usage Ingestion & Normalization** *(pipeline)* — raw usage intake, normalization to `UsageRecord`, continuous-duration session merge, authoritative usage dedup, correction ingestion. (PRD §9.2, §6.1)
- [`13-q-store-attribution.md`](./13-q-store-attribution.md) — **Windowed Q Store & Attribution** *(pipeline)* — the windowed counter `Q` (single-writer per `(subscription, meter, dimensionKey, window)`, SEAMS M7), per-slice attribution + `bandOffsetQ` (T-D-12), re-materialization and reversal decrements. (PRD §9.2)
- [`14-unit-synthesis-period-tick.md`](./14-unit-synthesis-period-tick.md) — **Unit Synthesis & Period Tick** *(pipeline)* — synthesis of the three evaluation-unit kinds incl. period-driven units at anchor boundaries (T-D-15), frozen-context assembly under the read-model pin discipline, cascade re-resolution routing (T-D-10 / T-D-12). (PRD §9.2)
- [`15-rated-output-balance-effects.md`](./15-rated-output-balance-effects.md) — **Rated Output, Delta Dedup & Balance Effects** *(pipeline)* — rated-output persistence, the outcome → RatedCharge/BillableItem mapping (ratifies slice 11 §4.1), delta dedup by correction key (T-D-11), `CommitmentBalanceEffect` publication to Contracts (T-D-10). (PRD §9.2)
- [`16-billing-handoff-operations.md`](./16-billing-handoff-operations.md) — **Billing Handoff & Operations** *(pipeline)* — the outbound Billing contract (full-precision amounts + obligations + rounding-policy id), `periodState` relay, partitioning/backpressure/replay topology, NFR verification. (PRD §9.2, §7.1)

## Slice map (PRD §6 / §17.1 step ↔ slice)

| Doc | PRD §6 | Seams owned |
|-----|--------|-------------|
| `01-foundation` | §6.1, §17.1 core | K1, K3, S1, M7, W2, M11 |
| `02-selection-eligibility` | §6.3 (steps 1-2), §6.5 | K1, K2, K4, K5, F1 |
| `03-metering-models` | §6.2, §6.5, §6.7 | M1, M2, M3, M5, M6, M7, M10, M11 |
| `04-overlays-precedence` | §6.4 (steps 4-5) | O1, O2, O3 |
| `05-commitments-reservations` | §6.6 (step 6) | M8, M9 |
| `06-coupons` | §6.8 (step 7) | (no scope-key seam) |
| `07-currency-fx` | §6.9 (step 8) | S1 (fx-lock segment) |
| `08-retroactivity-corrections` | §6.10 | W2 |
| `09-period-plan-change` | §6.11 | P1, P2 |
| `10-governance-asc606` | §6.12 | G1, B1 (rev-share pass-through) |
| `11-consumer-contracts` | §9 | C1, S1, B1 |
| `12-usage-ingestion-normalization` *(pipeline)* | §9.2 | — |
| `13-q-store-attribution` *(pipeline)* | §9.2 | M7 (writer side) |
| `14-unit-synthesis-period-tick` *(pipeline)* | §9.2 | — (T-D-15) |
| `15-rated-output-balance-effects` *(pipeline)* | §9.2 | — (T-D-10/11) |
| `16-billing-handoff-operations` *(pipeline)* | §9.2 | — |

The numeric prefix follows the **§17.1 evaluation order** (steps 1-9), then the period-level and
cross-cutting slices; slices 12–16 are the **operational pipeline** (ADR-0002). See
[`../DESIGN.md`](../DESIGN.md) for the dependency graph.
