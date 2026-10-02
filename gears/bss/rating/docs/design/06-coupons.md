Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-01 by Virtuozzo International GmbH

<!-- CONFLUENCE_TITLE: [BSS]: Rating — Coupons (Design) -->
<!-- Related: ../PRD.md, ../DESIGN.md, ../SEAMS.md | Upstream: Promotions (not yet a gear) | Downstream: step 8–9, Billing/Tax (discount lineage) | Owners: BSS Rating team -->

# DESIGN — Coupons (Slice 6)

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-design-coupons`

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
  - [4.1 Placement and the FX Split (normative)](#41-placement-and-the-fx-split-normative)
  - [4.2 Stacking Policies (normative)](#42-stacking-policies-normative)
  - [4.3 applyScope Attachment and Hybrid Split-Back (normative)](#43-applyscope-attachment-and-hybrid-split-back-normative)
  - [4.4 Frozen Coupon Snapshot and Fail-Closed Rules (normative)](#44-frozen-coupon-snapshot-and-fail-closed-rules-normative)
  - [4.5 Snapshot Segment and Discount Lineage (normative)](#45-snapshot-segment-and-discount-lineage-normative)
- [5. Traceability](#5-traceability)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

This slice is the **step-7 evaluator** in `rating-core`: it applies frozen coupon snapshots to the
post-commitment line amount, split around FX (price-currency coupons before step 8,
billing-currency coupons after). Rating owns application semantics only; coupon lifecycle,
campaigns and redemption are the Promotions domain's (PRD §5.2).

No Promotions gear or PRD exists (SEAMS G-3, R-11), so no coupon snapshot can enter the
`EvaluationInput`. **Launch behaviour**: the coupon set is empty and step 7 passes the amount
through unchanged. A pricing price row carrying `discountRef` fails the child closed with
`coupon_source_unavailable` — a referenced discount is never silently dropped. The rules in §4 are
the specified target behaviour and activate when a coupon-snapshot source is added.

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-bss-rating-fr-coupon-application-order` | Step 7 after steps 4–6; billing-currency pass after step 8 (§4.1). |
| `cpt-cf-bss-rating-fr-coupon-stacking` | `exclusive_best` default, `ordered_stack` for campaign-linked sets; fail-closed set (§4.2, §4.4). |
| `cpt-cf-bss-rating-fr-hybrid-pricing` | `applyScope` attachment; `line_total` fails closed at launch (T-D-22, §4.3). |
| `cpt-cf-bss-rating-fr-snapshot-carry` | Applied coupon ids + stacking policy recorded in the snapshot body (§4.5). |
| `cpt-cf-bss-rating-fr-evaluation-order` | Fixed step-7 slot. |

#### NFR Allocation

| NFR theme | Allocated To | Design Response | Verification / Status |
|-----------|--------------|-----------------|-----------------------|
| `cpt-cf-bss-rating-nfr-throughput-latency` | Step-7 evaluator | In-memory over input snapshots | Load test |
| `cpt-cf-bss-rating-nfr-horizontal-scale` | Stateless | No redemption state read or written | Design |
| `cpt-cf-bss-rating-nfr-resilience` | Fail-closed guards | Missing policy fields or unavailable source ⇒ typed error | Fixture |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|------------------|
| `cpt-cf-bss-rating-adr-scope-key-adoption` | Context only — the discounted rows resolve on the canonical key upstream. |

### 1.3 Architecture Layers

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-tech-stack-cpn`

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| Domain | `EligibilityFilter`, `StackingResolver`, `ScopeAttacher`, two application passes | `rating-core` module |
| Infrastructure | None | — |

## 2. Principles and Constraints

### 2.1 Design Principles

#### Apply, never own

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-principle-apply-never-own-cpn`

Rating applies coupon snapshots and records the result; it never creates, counts or expires a
redemption.

#### Policy from the snapshot only

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-principle-policy-from-snapshot-cpn`

Scope, stacking, sequence, validity and settlement currency come from snapshot fields; an absent
field fails closed.

#### Lineage is part of the outcome

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-principle-lineage-first-cpn`

Pre/post-coupon amounts and applied ids are recorded per line and pass so Billing/Tax can choose
gross-vs-net treatment.

### 2.2 Constraints

#### Fixed slot, compiled FX split

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-constraint-fixed-slot-cpn`

Price-currency coupons at step 7, billing-currency coupons after step 8; no configuration changes
the placement.

#### No redemption mutation, snapshot-only replay

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-constraint-no-redemption-mutation-cpn`

The applied snapshots are stored with the window result; a replay uses them, never live Promotions
state.

#### Promotions contract maturity

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-constraint-promotions-maturity-cpn`

`[DEPENDENCY GAP R-11]` No Promotions PRD or gear; PRD §17.2 is the Rating-side field list. Field
names and the snapshot delivery contract must be agreed before coupons can be rated.

## 3. Technical Architecture

### 3.1 Domain Model

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-domain-model-cpn`

- **`CouponSnapshot`** — `couponId`, `adjustmentType` (`percent | fixed_amount`), `value`,
  `valueCurrency` (required for `fixed_amount`), `settlementCurrency` (`price | billing`),
  `applyPerTierBand`, `applyScope` (`usage | recurring | line_total`), `stackSequence` (required and
  unique under `ordered_stack`), validity, applicability filters, redemption eligibility.
- **`StackingPolicy`** — `exclusive_best | ordered_stack`.
- **`CouponApplication`** — coupon id, pass, basis, discount, result.
- **`DiscountLineage`** — per line and pass: pre/post amounts, applied ids, policy.

### 3.2 Component Model

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-component-coupon-evaluator-cpn`

- **`Step7Evaluator`** — `discountRef` guard, both passes, fail-closed checks.
- **`EligibilityFilter`** — validity at the slice start, applicability, redemption eligibility.
- **`StackingResolver`** — §4.2.
- **`ScopeAttacher`** — §4.3.

### 3.3 API Contracts

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-interface-step7-evaluator-cpn`

Internal: `apply_coupons(&Step6Line, &[CouponSnapshot], Pass) -> Result<CouponLine,
EvaluationError>`, called once with `Pass::Price` at step 7 and once with `Pass::Billing` after
step 8. Errors: `coupon_source_unavailable`, `coupon_policy_missing`, `coupon_sequence_invalid`,
`coupon_currency_mismatch`, `coupon_scope_unsupported` (`line_total`), `coupon_incompatible_pair`,
`coupon_comparison_undefined`.

### 3.4 Internal Dependencies

Upstream: slice 05 (post-commitment amount), slice 03 (band amounts for `applyPerTierBand`),
slice 04 (partner discounts coexist). Downstream: slice 07 (conversion between the passes).

### 3.5 External Dependencies

| Dependency | What arrives | Status |
|------------|--------------|--------|
| Promotions | Coupon snapshots | MISSING — SEAMS G-3 (R-11) |
| Pricing | `discountRef` on the price row | Field exists (SEAMS P-2); triggers the fail-closed guard |
| Billing / Tax | Consume discount lineage | MISSING gear (R-05) |

### 3.6 Interactions and Sequences

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-step7-line-cpn`

1. Guard: row has `discountRef` and no coupon source ⇒ `coupon_source_unavailable`.
2. Filter candidates; any missing policy field fails closed (§4.4).
3. Partition by `settlementCurrency`; apply the `price` set now.
4. Resolve stacking (§4.2) and attach per `applyScope` (§4.3).
5. Record lineage; continue to step 8.

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-billing-currency-pass-cpn`

After step 8, apply the `billing` set to the billing-currency amount with the same rules; then the
step-9 guards run. With native currency (launch) both passes operate on the same currency, in the
same fixed order.

### 3.7 Database Schemas and Tables

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-storage-none-cpn`

None. Applied snapshots and lineage are stored in the window result.

### 3.8 Deployment Topology

- [ ] `p3` - **ID**: `cpt-cf-bss-rating-deployment-cpn`

Part of `rating-core`.

## 4. Additional Context

### 4.1 Placement and the FX Split (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-placement-fx-split-cpn`

- Order: catalog base → overlays (4) → contract (5) → commitment (6) → coupon (7) → FX (8) → emit (9).
- `settlementCurrency = price`: step 7, price currency, before FX. `settlementCurrency = billing`:
  after step 8 on the billing-currency amount, same FX rate record.
- Coupons and partner overlays both apply; neither excludes the other.

### 4.2 Stacking Policies (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-stacking-cpn`

- `exclusive_best` (default): one coupon per line — the lowest resulting charge. When a `line_total`
  candidate is present exclusivity widens to one coupon per plan per period (T-D-20); inert while
  `line_total` fails closed.
- `ordered_stack`: only for campaign-linked sets; folds ascending `stackSequence`, each step on the
  prior output. Missing or duplicate `stackSequence` fails closed.
- Incompatible pairs are rejected at redemption bind (Promotions); if both arrive applicable,
  evaluation fails closed.
- `[OPEN QUESTION]` Equal-benefit tie-break (default proposal: ascending `couponId`).
- `[OPEN QUESTION]` Mixed-settlement candidate sets under either policy — fail closed until pinned
  with Promotions/Finance.

### 4.3 applyScope Attachment and Hybrid Split-Back (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-applyscope-cpn`

- `usage` / `recurring`: bind to that line.
- `line_total`: fails closed at launch (`coupon_scope_unsupported`, T-D-22) — usage and recurring
  lines are different units rated on different triggers, so no evaluation holds a plan's lines
  together. Target semantics once a plan-period assembly point exists: apply once to the combined
  total, split back pro-rata to pre-coupon amounts at full precision; if both are 0 assign the
  (zero) discount to the recurring line. `[OPEN QUESTION]` Assembly point, trigger and cascade —
  joint with Promotions.
- `applyPerTierBand = false`: discount the line total after tier math; `true`: per marginal band;
  on unbanded lines `true` behaves as total and is recorded as such.

### 4.4 Frozen Coupon Snapshot and Fail-Closed Rules (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-fail-closed-snapshot-cpn`

Fail closed (never defaulted): coupon source unavailable while `discountRef` is set; missing
`applyScope`; missing/duplicate `stackSequence` under `ordered_stack`; missing `valueCurrency` on
`fixed_amount`; a `fixed_amount` whose `valueCurrency` does not match its pass's currency; unknown
`adjustmentType`/`settlementCurrency`; `applyScope = line_total`; the §4.2 opens. Eligibility is
evaluated against snapshot fields only.

### 4.5 Snapshot Segment and Discount Lineage (normative)

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-normative-segment-lineage-cpn`

- Applied coupon ids + stacking policy are a segment of the `rating_snapshot` body (DESIGN §4.1);
  empty at launch.
- Lineage per line and pass: pre/post amounts, applied ids. Whether a contractual floor claws back a
  coupon discount is a PRD §15 open; the lineage keeps either answer computable.

## 5. Traceability

**Traces to**: `cpt-cf-bss-rating-fr-coupon-application-order`, `cpt-cf-bss-rating-fr-coupon-stacking`

- **PRD**: §6.8, §6.2 (hybrid attachment), §17.1 steps 7–8, §17.2.
- **Seams / contracts**: [`../SEAMS.md`](../SEAMS.md) G-3, P-2.
- **Decisions**: T-D-20, T-D-22; R-11 — [`../DECISIONS.md`](../DECISIONS.md).
- **Related slices**: [`05-commitments-reservations.md`](./05-commitments-reservations.md), [`07-currency-fx.md`](./07-currency-fx.md), [`09-period-plan-change.md`](./09-period-plan-change.md).
