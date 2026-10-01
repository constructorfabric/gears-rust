Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-01 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Commitments & Reservations (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md | Upstream: Pricing (reservation attributes), Contracts & Agreements and reservation entitlement source (not yet gears) | Downstream: steps 7–9, Billing (obligations) | Owners: BSS Rating team -->

# DESIGN — Commitments & Reservations (Slice 5)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-commitments-reservations`

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
  - [4.1 Commitment-Pool Waterfall (normative)](#41-commitment-pool-waterfall-normative)
  - [4.2 Reservation Flavors and Pool Precedence (normative)](#42-reservation-flavors-and-pool-precedence-normative)
  - [4.3 Reserved-Rate Two-Source Rule (normative)](#43-reserved-rate-two-source-rule-normative)
  - [4.4 Commitment Pool vs Prepaid Credit Grant (normative)](#44-commitment-pool-vs-prepaid-credit-grant-normative)
  - [4.5 Obligations, Reversals, and Period Boundaries (normative)](#45-obligations-reversals-and-period-boundaries-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice is the **step-6 evaluator** in `rating-core`. Commitments and reservations are
compositions over the steps 2–5 output, not model kinds (T-D-05): they split a line's quantity into
reserved, in-commit and overage portions priced at already-resolved rates, and surface a structured
`TrueUpObligation` for Billing.

Two inputs are needed and neither has a source today:

- **Reservation match** — which reservation covers which usage or allocated quantity
  (`reservedQuantity`, coverage interval). Pricing publishes the *rates*
  (`reservedRateNanoMinor`, `reservationFlavor ∈ {Consumption, Capacity}` on the usage row —
  SEAMS P-2), but no gear supplies the match.
- **Commitment pools** — ordered `commitmentPools[]` with balances, owned by Contracts, which is not
  a gear yet (SEAMS G-2, R-11).

Launch behaviour therefore is: a price row that carries `reservationFlavor` fails the child closed
with `reservation_match_unavailable`; commitment pools are never evaluated (no pools in the
input). The rules in §4 are the specified target behaviour and activate when the sources are added
to the `EvaluationInput`.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-commitment-drawdown` | Waterfall over ordered pools (§4.1) — activates when the Contracts gear supplies pools (R-11). |
| `cpt-cf-bss-rating-fr-committed-usage` | In-commit / overage split; `TrueUpObligation` for `committed_rate` pools (§4.5) — activates with R-11. |
| `cpt-cf-bss-rating-fr-reservation-consumption-flavor` | Matched quantity at the reserved rate, remainder re-banded (§4.2) — requires a match source. |
| `cpt-cf-bss-rating-fr-capacity-charge` | `capacityCharge = reservedRate × reservedQuantity × coveredGranules` as a period-driven line (§4.2) — requires a match source. |
| `cpt-cf-bss-rating-fr-hybrid-pricing` | Commitment attaches to the usage line unless the plan marks it plan-level. |
| `cpt-cf-bss-rating-fr-evaluation-order` | Fixed step-6 slot; intra-step order reservation → waterfall → overage. |

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-throughput-latency` | Step-6 evaluator | Pure arithmetic over input values | Load test |
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Input freezing | Pool balances enter the input as values; no live balance read or lock | Design |
| `cpt-cf-bss-rating-nfr-resilience` | Fail-closed guards | Missing match, unknown flavor, torn pool input ⇒ typed error | Fixture |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | The rates step 6 composes over resolve on the canonical key; this slice adds no selection axis. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-cmt`

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain | `ReservationMatcher`, `PoolWaterfall`, `TrueUpAssembler` | `rating-core` module |
| Infrastructure | None | — |

## 2. Principles and Constraints

### 2.1 Design Principles

#### Composition, not a model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-composition-cmt`

Step 6 partitions a line's quantity and prices the partitions at rates resolved in steps 2–5; it adds
no formula beyond the reservation tier-counter exclusion (§4.2).

#### Frozen balances, thin evaluation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-frozen-balances-cmt`

Pool balances, draw order, rollover and flavor are values in the `EvaluationInput`, recorded in the
window result; the core never reads or writes a live balance.

#### Surface, never post

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-surface-not-post-cmt`

Step 6 produces effects (lineage) and obligations (`TrueUpObligation`), never a balance mutation or
posting.

### 2.2 Constraints

#### Fixed slot, fixed intra-step order

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-fixed-slot-cmt`

Always step 6; reservation precedes pools; overage is the residual. No configuration changes it.

#### Reversal math is slice 08's

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-reversal-boundary-cmt`

A correction re-evaluates the child and produces a new revision (slice 08); this slice records
per-pool draws on every revision so a Contracts consumer can compute the balance effect.

#### Launch posture: single pool first

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-constraint-launch-posture-cmt`

When pools arrive, the first release evaluates the single-pool case; multi-pool waterfall and
rollover use the same input shape (PRD §17.4).

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-cmt`

- **`ReservationMatch`** — `match_id`, `flavor` (`Consumption | Capacity`), `reserved_quantity`,
  coverage sub-intervals `[(from, to, reserved_quantity)]`, rate source (catalog row or contract
  overlay). No source today.
- **`CommitmentPoolInput`** — per pool: `pool_id`, contract ref, unit (quantity | spend),
  `pool_type ∈ {prepaid_drawdown, committed_rate}`, remaining balance, `balance_version`, draw order,
  rollover policy, optional `overage_rate`. No source today.
- **`CommitmentEffect`** — per-pool draws in order, in-commit vs overage split, applied rates.
- **`ReservationEffect`** — matched quantity at the reserved rate, `capacityCharge`, exclusions
  applied.
- **`TrueUpObligation`** — `(amount: ExactAmount, currency, period_start, contract_ref)`; executed by
  Billing.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-commitment-evaluator-cmt`

- **`Step6Evaluator`** — input guards and intra-step order.
- **`ReservationMatcher`** — consumption split / capacity charge (§4.2).
- **`PoolWaterfall`** — ordered drawdown (§4.1).
- **`TrueUpAssembler`** — period-end obligation on the `period_line` child (§4.5).

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-step6-evaluator-cmt`

Internal: `apply_step6(&OverlaidLine, Option<&ReservationMatch>, &[CommitmentPoolInput]) ->
Result<Step6Line, EvaluationError>`. Errors: `reservation_match_unavailable` (row has
`reservationFlavor`, input has no match), `reservation_rate_unresolved`, `unknown_pool_type`,
`pool_currency_mismatch`, `pool_input_torn`.

### 3.4 Internal Dependencies

Upstream: slice 03 (remainder band math), slice 04 (post-overlay amount; negotiated reserved rate via
step 5). Downstream: slice 06 (post-commitment amount), slice 08 (differences between versions),
slice 09 (`period_line` children, plan-change reset).

### 3.5 External Dependencies

| Dependency | What arrives | Status |
|------------|--------------|--------|
| Pricing | `reservedRateNanoMinor`, `reservationFlavor` on the usage row; `includedAllowance` | Fields exist in the plan document (SEAMS P-2); read API MISSING (R-02) |
| Reservation entitlement source (OSS / Contracts) | `ReservationMatch` | MISSING — no gear (R-11) |
| Contracts & Agreements | pools, true-up clause, negotiated reserved rates | MISSING — SEAMS G-1, G-2 (R-11) |
| Billing | executes `TrueUpObligation` | MISSING — no gear (R-05) |

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-step6-line-cmt`

1. Guard: row has `reservationFlavor` and no match ⇒ `reservation_match_unavailable`.
2. Capacity flavor: emit the capacity line (§4.2) on the `period_line` child.
3. Consumption flavor: split matched vs remainder; the remainder re-runs steps 3–5 (T-D-13).
4. Waterfall over pools (when present); residual priced per the `overage_rate` selector (§4.1).
5. Record effects in lineage; continue to step 7.

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-trueup-period-cmt`

On the `period_line` child for a `committed_rate` pool: compute the shortfall (§4.5); emit a
`TrueUpObligation` in the outcome's `obligations`; zero shortfall emits nothing.

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-none-cmt`

None. Effects and obligations are stored in `rating_window_result.lines` / `.obligations` (DESIGN §3.7).

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-cmt`

Part of `rating-core`.

## 4. Additional Context

### 4.1 Commitment-Pool Waterfall (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-waterfall-cmt`

*Activates when the Contracts gear supplies pools — R-11.*

- Pools are drawn in declared order; each absorbs quantity/spend up to its remaining balance; the
  residual is overage.
- Residual pricing (T-D-19): `overage_rate` present ⇒ flat rate, no band math; absent ⇒ banded
  on-demand rates over the post-reservation remainder quantity (pool draws do not reduce it).
- Billability (T-D-14): `prepaid_drawdown` ⇒ in-commit line due 0 with notional value in lineage;
  `committed_rate` ⇒ in-commit bills in arrears at the in-commit rate.
- Spend pools draw in price currency; a pool denominated in another currency fails closed
  (`pool_currency_mismatch`).
- Tier counter: in-commit quantity is **not** excluded from `Q` (only reservations are).
- Pool balances are values of the input and are recorded on the window result. Cross-unit balance
  sequencing and `CommitmentBalanceEffect` publication (T-D-10, T-D-27) are dormant until a Contracts
  consumer exists.

### 4.2 Reservation Flavors and Pool Precedence (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-reservation-flavors-cmt`

*Requires a reservation-match source — R-11.*

- **Consumption flavor**: matched quantity prices at the reserved rate; the remainder prices at
  on-demand rates from steps 2–5. The matched quantity is excluded from pool drawdown and from the
  on-demand tier counter: the remainder is banded on a window-cumulative remainder axis starting at
  the window origin (`remainderOffset` across slices — T-D-23), never reset per slice. Steps 3–5
  re-run over the remainder (T-D-13).
- **Capacity flavor**: a period-driven line `capacityCharge = reservedRate × reservedQuantity ×
  coveredGranules`, emitted regardless of usage, never drawing pools (T-D-25). `reservedRate` is
  per billable-unit granule; `coveredGranules` is the reservation's covered duration within the
  billing period, summed per sub-interval when coverage or quantity changes.

Worked example (capacity, `reservedRateNanoMinor` = 20 000 000 = €0.0002 per GB·h, hour granule,
30-day period = 720 h):

```text
100 GB reserved for the whole period       100 × 720 × 0.0002       = €14.40
allocated on day 20 (11 days = 264 h)      100 × 264 × 0.0002       = € 5.28
100 GB days 1–24, 200 GB days 25–30        100×576×0.0002 + 200×144×0.0002 = 11.52 + 5.76 = €17.28
```

- No match in the input and no `reservationFlavor` on the row ⇒ pure usage pricing.

### 4.3 Reserved-Rate Two-Source Rule (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-reserved-rate-sourcing-cmt`

- Self-service reserved rates: the pinned plan document (`reservedRateNanoMinor` on the usage row).
- Negotiated reserved rates: the step-5 contract overlay (no source at launch).
- Step 6 uses the post-step-5 value; a match whose rate resolves from neither source fails closed
  (`reservation_rate_unresolved`).

### 4.4 Commitment Pool vs Prepaid Credit Grant (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-pool-vs-grant-cmt`

| | Commitment pool | Prepaid credit grant |
|---|---|---|
| Construct | `commitmentPools[]`, step-6 waterfall | Plan-attached wallet (`grantAmount`, `creditUnit`, `expiryPolicy`) |
| Definition owner | Contracts | Pricing |
| Balance owner | Contracts | Billing (ledger `CreditGrant` / `CreditApply` exist in `bss-ledger`) |
| Drawdown | Rating, step 6 | Billing, outside the per-line order |

Step 6 never draws a wallet grant. Rating documents never use the bare word "prepaid".

### 4.5 Obligations, Reversals, and Period Boundaries (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-obligations-boundary-cmt`

- `TrueUpObligation` shape: `(amount: ExactAmount, currency = price currency, period_start, contract_ref)`.
- `committed_rate` quantity basis: `max(0, committedQuantity − consumedInCommitQuantity) ×
  in-commit rate`; spend basis: `max(0, committedSpend − inCommitBilledAmount)`.
  `prepaid_drawdown` pools emit no true-up.
- The waterfall cannot produce a negative component; the non-negative guard runs at step 9.
- Plan change: pool carry defaults to reset; pricing publishes no pool flag (T-D-29).

## 5. Traceability

**Traces to**: `cpt-cf-bss-rating-fr-commitment-drawdown`, `cpt-cf-bss-rating-fr-reservation-consumption-flavor`, `cpt-cf-bss-rating-fr-capacity-charge`,
`cpt-cf-bss-rating-fr-committed-usage`

- **PRD**: §6.2, §6.6, §17.1 step 6, §17.3, §17.4.
- **Seams / contracts**: [`../SEAMS.md`](../SEAMS.md) P-2, G-1, G-2.
- **Decisions**: T-D-05, T-D-08, T-D-09, T-D-10, T-D-13, T-D-14, T-D-19, T-D-23, T-D-25, T-D-27, T-D-29; R-11 — [`../DECISIONS.md`](../DECISIONS.md).
- **Related slices**: [`03-metering-models.md`](./03-metering-models.md), [`04-overlays-precedence.md`](./04-overlays-precedence.md), [`08-retroactivity-corrections.md`](./08-retroactivity-corrections.md), [`09-period-plan-change.md`](./09-period-plan-change.md).
