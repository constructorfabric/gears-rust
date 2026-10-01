Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-01 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Overlays & Precedence (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md | Upstream: Pricing (overlay documents), Contracts & Agreements (not yet a gear) | Downstream: steps 6–9 | Owners: BSS Rating team -->

# DESIGN — Overlays & Precedence (Slice 4)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-overlays-precedence`

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
  - [4.1 Scope → Tenant-Axis Mapping (normative)](#41-scope--tenant-axis-mapping-normative)
  - [4.2 Stacking and the Total Order (normative)](#42-stacking-and-the-total-order-normative)
  - [4.3 Contract Overlay Precedence (normative)](#43-contract-overlay-precedence-normative)
  - [4.4 Bounded Composition — Anti-Drift Cap (normative)](#44-bounded-composition--anti-drift-cap-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice is the **steps 4–5 evaluator** in `rating-core`. Step 4 takes the step-3 line amount,
selects every pricing `PriceOverlay` whose scope matches the evaluation context, and applies **all**
of them as one sequential stack in a total order. Step 5 then applies the customer/contract overlay
(`Contract > Partner price overlays > Catalog base`). The cumulative effect is bounded by the
anti-drift cap.

Overlays are read from the **overlay documents of the child's pinned `catalog_version`**
(DESIGN §4.1; SEAMS P-2), stored in `rating_catalog_document`. Overlay authoring and publish-time
precedence validation are the pricing gear's; this slice only evaluates. Contract overlays have no
source gear yet (SEAMS G-1, R-11): at launch step 5 has no input and is a no-op.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-priceoverlay-scope-mapping` | `ScopeFilter` applies the fixed scope → tenant-axis table (§4.1); `resourceTenantId` alone never matches a partner/orgTier overlay. |
| `cpt-cf-bss-rating-fr-overlay-stacking` | `OverlayStacker` applies all survivors in the total order ascending `precedence` → class order → `priceOverlayId` (§4.2). |
| `cpt-cf-bss-rating-fr-customer-contract-overlay` | `ContractOverlayApplier` runs after step 4; no input until a Contracts source exists (§4.3). |
| `cpt-cf-bss-rating-fr-bounded-composition-cap` | `CompositionCapGuard` clamps-and-records or fails closed against `maxCumulativeMarkup` (§4.4). |

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-throughput-latency` | `ScopeFilter`, `OverlayStacker` | In-memory over the pinned overlay documents already loaded for the child; no I/O in steps 4–5 | Load test |
| `cpt-cf-bss-rating-nfr-audit-segregation` | Lineage | Every applied line (overlay id, revision, line id, pre/post amount) is recorded in the rated line lineage | Fixture |
| `cpt-cf-bss-rating-nfr-resilience` | Total order + fail-closed problems | A catalog defect that publish validation missed still yields one deterministic order; unresolvable scopes fail closed | Fixture |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | The base row carries `price_overlay = base` on the canonical key; overlays are step-4 stack material, not selection candidates. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-ovl`

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain | `ScopeFilter`, `OverlayStacker`, `ContractOverlayApplier`, `CompositionCapGuard`, lineage | `rating-core` module (pure Rust) |
| Infrastructure | None — overlay documents arrive in the `EvaluationInput` | — |

## 2. Principles and Constraints

### 2.1 Design Principles

#### Stack all survivors; tie-break, don't exclude

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-stack-not-winner-ovl`

Every scope-matching overlay contributes to the stack (T-D-02). The class-specificity order resolves
ordering between classes; it never removes an overlay from the stack.

#### One total order, no arbitrary picks

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-total-order-ovl`

Ascending `precedence`, then the class order for cross-class ties, then ascending `priceOverlayId`
within a class. The same input always stacks in the same order.

#### Axes are typed, not inferred

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-principle-typed-axes-ovl`

Each scope class matches exactly one context axis (§4.1). A value that cannot be resolved from the
evaluation input fails the child closed (`unresolvable_scope`); it never matches by similarity.

### 2.2 Constraints

#### Publish-side validation is relied on, not re-run

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-publish-side-guarantees-ovl`

Pricing rejects equal `precedence` among overlapping overlays within one class, and absolute-amount
lines that do not cover the priced row's currency at publish. The runtime order is a safety net; an
observed within-class precedence tie is recorded in lineage (`precedence_tie`) and still resolves by
`priceOverlayId`.

#### The overlay segment is sealed

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-overlay-segment-ovl`

The applied overlay ids and revisions are written into the line's `rating_snapshot` body
(DESIGN §4.1). A replay of a window result reads the same overlay documents from the recorded
`catalog_version`.

#### Caps clamp or fail — never silently compound

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-constraint-cap-modes-ovl`

A stacked result beyond `maxCumulativeMarkup` clamps and records, or fails closed under a hard cap.
The default cap value and mode are a PRD §15 open.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-domain-model-ovl`

- **`OverlayDocument`** — pricing overlay subject at the pinned version: `priceOverlayId`, `revision`,
  `scopeClass` (`Global | Region | Brand | OrgTier | Partner | CustomerGroup`), `scopeValue`,
  `precedence`, `effectiveFrom`, `effectiveTo`, `targetPlans`, `lines[]`.
- **`OverlayLine`** — `lineId`, `planId`, `targetSku` (optional), `cohort` (optional), `kind`
  (`Markup | Discount | Fixed`), `magnitudeKind` (`PercentBp | Amount`), `percentBp`,
  `amounts[{currency, minor}]`.
- **`StackOrderKey`** — `(precedence asc, class order, priceOverlayId asc)`.
- **`OverlayLineage`** — per applied layer: overlay id + revision, line id, kind, magnitude,
  pre-amount, post-amount; clamp record if any.

Amounts inside the stack are exact rationals in minor units (T-D-46); a pricing absolute amount
(`minor`) enters as an integer; `percentBp` is basis points of the running amount (exact).

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-component-overlays-precedence-ovl`

- **`ScopeFilter`** — keeps overlays effective at the slice start (`[effectiveFrom, effectiveTo)`)
  whose `targetPlans` include the priced plan and whose scope matches per §4.1.
- **`OverlayStacker`** — orders survivors by `StackOrderKey`, selects each overlay's most-specific
  line for the priced row, applies it to the running amount (§4.2).
- **`ContractOverlayApplier`** — step 5 (§4.3).
- **`CompositionCapGuard`** — §4.4.

### 3.3 API Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-interface-stack-overlays-ovl`

Internal: `apply_overlays(&ModelLineOutcome, &[OverlayDocument], &ScopeContext) ->
Result<OverlaidLine, EvaluationError>`. Errors: `unresolvable_scope`, `invalid_overlay_line`
(e.g. an unknown magnitude kind), `composition_cap_exceeded`, `contract_dimension_violation`
(defensive). A clamp is a recorded outcome, not an error; an absolute line without an amount for the
line currency is dropped with `overlay_currency_uncovered` (§4.2), not an error.

### 3.4 Internal Dependencies

Upstream: slice 01 (pipeline, lineage), slice 02 (selected row: `plan_id`, `sku_id`, `cohort`,
currency), slice 03 (step-3 line amount). Downstream: slice 05 (step 6 uses the overlaid amount;
a reservation split re-runs steps 3–5 over the remainder), slice 06, slice 07.

### 3.5 External Dependencies

| Dependency | What arrives | Status |
|------------|--------------|--------|
| Pricing | Overlay documents at the pinned `catalog_version` | MISSING read API — SEAMS P-2, R-02 |
| Pricing | Customer-group membership for `customerGroup` scope | `[DEPENDENCY GAP R-02]` — not exposed by the pricing SDK |
| Subscriptions | `sellerTenantId`, `payerTenantId`, `brandId`, region binding in the subscription version | ASSUMED — SEAMS S-1 |
| Contracts & Agreements | Contract/account overlay terms | MISSING — SEAMS G-1, R-11 |

### 3.6 Interactions and Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-stack-overlays-ovl`

1. `ScopeFilter` evaluates every overlay document of the pinned version against §4.1.
2. `OverlayStacker` sorts survivors by `StackOrderKey`; for each, picks the line and applies it.
3. `ContractOverlayApplier` applies contract terms (none at launch).
4. `CompositionCapGuard` checks the cumulative change against the cap.
5. Applied ids go to lineage and the snapshot body; the line proceeds to step 6.

Worked example (price currency EUR, slice amount after step 3 = €100.00):

```text
overlay A  scope partner = seller S1, precedence 10, line Markup  +1000 bp  → 100.00 × 1.10 = 110.00
overlay B  scope region  = eu-west,   precedence 20, line Discount −500 bp  → 110.00 × 0.95 = 104.50
overlay C  scope partner = seller S2, precedence  5                          → filtered out (seller ≠ S2)
result 104.50 EUR = exact 10 450/1 minor; lineage [A, B]
```

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-cap-clamp-ovl`

If the stack exceeds a `clamp` cap, the amount is set to the cap bound and the pre-clamp amount is
kept in lineage; under a `hard` cap the child fails with `composition_cap_exceeded`.

### 3.7 Database Schemas and Tables

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-storage-none-ovl`

None. Overlay documents are stored by the pipeline in `rating_catalog_document` (DESIGN §3.7); the
applied set is persisted in the window result lineage and snapshot.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-ovl`

Part of `rating-core`; nothing slice-specific.

## 4. Additional Context

### 4.1 Scope → Tenant-Axis Mapping (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-scope-mapping-ovl`

| `scopeClass` | Matches | Source in the evaluation input |
|--------------|---------|--------------------------------|
| `Global` | always (subject to `targetPlans`) | — |
| `CustomerGroup` | payer's customer group at the slice start | `[DEPENDENCY GAP R-02]` — no membership source; until pricing exposes it, a `CustomerGroup` overlay targeting the priced plan fails the child closed (`unresolvable_scope`) |
| `Partner`, `OrgTier` | `sellerTenantId` | subscription version |
| `Brand` | plan/SKU `brandId` | subscription version / plan document |
| `Region` | price-row `region` | selected row's scope key |

`resourceTenantId` never qualifies a partner/orgTier overlay; `payerTenantId` is used only in
step 5.

### 4.2 Stacking and the Total Order (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-stacking-order-ovl`

- All survivors apply sequentially; layer *n+1* sees layer *n*'s output.
- Order: ascending `precedence` → class order `CustomerGroup > Partner > OrgTier > Brand > Region >
  Global` for cross-class ties → ascending `priceOverlayId`.
- **Line selection**: an overlay is a line container; it contributes exactly one line — the most
  specific for the priced row: `(planId, targetSku)` > `(planId)` > list default. An overlay whose
  lines all miss the priced row contributes nothing.
- **Cohort filter** (pricing D-78, T-D-30): a line with `cohort` unset does not apply to
  `existing_grandfathered` rows; a line with `cohort = C` applies only to rows of generation `C`.
- **Adjustment kinds** (pricing D-138): `Markup` adds (percent of the running amount, or the absolute
  amount), `Discount` subtracts, `Fixed` replaces the running amount with the absolute amount.
- **Absolute amounts** apply the value for the row's price currency. If the line has no value for
  that currency (a currency added to the base row after the overlay was published — pricing flags it
  `coverage_incomplete`), the overlay is **dropped from the stack for that currency** and evaluation
  continues; lineage records `overlay_currency_uncovered`.
- Adjustments are charge-line level: on banded lines they apply to the post-band amount.
- **Reservation re-run** (T-D-13): when step 6 splits a consumption reservation, the same survivor
  set and order re-apply to the on-demand remainder amount; the reserved-rate portion is not
  re-overlaid.

### 4.3 Contract Overlay Precedence (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-contract-overlay-ovl`

- Step 5 applies contract/account overrides after step 4; contract terms outrank partner overlays.
- Overrides must not introduce dimensions absent from the published plan revision (contract publish
  rejects them; `contract_dimension_violation` is the defensive runtime error).
- Negotiated reserved rates arrive through this overlay and are consumed by step 6 (slice 05 §4.3).
- **Launch**: no Contracts gear exists (R-11); step 5 receives no terms and passes the amount
  through unchanged. The rules above activate when a Contracts source is added to the
  `EvaluationInput`.

### 4.4 Bounded Composition — Anti-Drift Cap (normative)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-normative-anti-drift-cap-ovl`

- The cumulative relative change of steps 4–5 over the step-3 amount is bounded by
  `maxCumulativeMarkup`.
- `clamp` mode: clamp to the bound, record pre-clamp amount; `hard` mode: fail closed.
- A chain of depth ≥ 2 without a configured cap is rejected at publish (validator 3, slice 10).
- `[OPEN QUESTION]` Default cap value and mode (PRD §15, Program/Finance). The pricing plan document
  does not yet carry `maxCumulativeMarkup`; until it does, no cap is evaluated and a depth ≥ 2 stack
  records `cap_not_configured` in lineage.

## 5. Traceability

**Traces to**: `cpt-cf-bss-rating-fr-priceoverlay-scope-mapping`, `cpt-cf-bss-rating-fr-overlay-stacking`, `cpt-cf-bss-rating-fr-customer-contract-overlay`,
`cpt-cf-bss-rating-fr-bounded-composition-cap`

- **PRD**: §6.3 `fr-priceoverlay-scope-mapping`; §6.4; §17.1 steps 4–5.
- **Seams / contracts**: [`../SEAMS.md`](../SEAMS.md) P-2, S-1, G-1, §I O1–O3.
- **Decisions**: T-D-02, T-D-13, T-D-30; R-02, R-11 — [`../DECISIONS.md`](../DECISIONS.md).
- **ADR**: [`../ADR/0001-cpt-cf-bss-rating-adr-scope-key-adoption.md`](../ADR/0001-cpt-cf-bss-rating-adr-scope-key-adoption.md).
- **Related slices**: [`03-metering-models.md`](./03-metering-models.md), [`05-commitments-reservations.md`](./05-commitments-reservations.md), [`10-governance-asc606.md`](./10-governance-asc606.md).
