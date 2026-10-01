# Feature: Price Adjustments: Overlays, Commitments, Coupons and FX

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-featstatus-price-adjustments-implemented`

- [ ] `p1` - `cpt-cf-bss-rating-feature-price-adjustments`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Apply the step-4 slots to one line](#21-apply-the-step-4-slots-to-one-line)
  - [Migrated namespace flows](#migrated-namespace-flows)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Stack price overlays](#31-stack-price-overlays)
  - [3.2 Enforce the composition cap](#32-enforce-the-composition-cap)
  - [3.3 Apply reservations and commitment pools](#33-apply-reservations-and-commitment-pools)
  - [3.4 Apply coupons](#34-apply-coupons)
  - [3.5 Resolve currency and convert](#35-resolve-currency-and-convert)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Step activation](#41-step-activation)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Overlays and the composition cap](#51-overlays-and-the-composition-cap)
  - [5.2 Reservations and commitments](#52-reservations-and-commitments)
  - [5.3 Coupons](#53-coupons)
  - [5.4 Currency roles and FX](#54-currency-roles-and-fx)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Detailed Behavior Contracts](#7-detailed-behavior-contracts)
  - [Overlays: Interactions and Sequences](#overlays-interactions-and-sequences)
  - [Commitments: Interactions and Sequences](#commitments-interactions-and-sequences)
  - [Coupons: Interactions and Sequences](#coupons-interactions-and-sequences)
  - [Currency and FX: Interactions and Sequences](#currency-and-fx-interactions-and-sequences)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Keep the price-adjustment steps of the evaluation order inside `rating-core` as **fail-closed
placeholders**. On `main` none of them has a source: Pricing removed `PriceOverlay` (PriceBook spec
item 2 — a plan-specific exception uses another book or SKU), Promotions are owned by Pricing and
deferred (pricing D-409), reserved capacity and prepaid credit are "later, by separate decision",
Contracts has a first-draft PRD only, and a price book has exactly one currency (pricing D-384).
In the evaluation order of PRD §17.1 (bind, meter, model, **step 4 dormant slots**, minimum-fee
floor, guards) these are the slots of step 4, in order: **4a** contract override, **4b** commitments
and reservations, **4c** coupons, **4d** FX. Price overlays (the former step 4) no longer exist; the
former steps 5–8 are slots 4a–4d and the former step 9 is step 6. All four slots run with empty
inputs, and a line whose input names any of them fails closed (T-D-73; R-07, R-11).

### 1.2 Purpose

These steps would change a model amount using inputs owned by Contracts, Promotions (Pricing) and
Finance. Keeping them one implementation and review boundary makes the launch posture — empty
inputs, typed fail-closed errors, never a default — testable on its own, and lets each step
activate as a new engine generation when its upstream source is delivered, without touching the
bind, meter and model steps or the pipeline.

**Requirements** (primary, per DECOMPOSITION): `cpt-cf-bss-rating-fr-priceoverlay-scope-mapping`, `cpt-cf-bss-rating-fr-overlay-stacking`, `cpt-cf-bss-rating-fr-customer-contract-overlay`, `cpt-cf-bss-rating-fr-bounded-composition-cap`, `cpt-cf-bss-rating-fr-committed-usage`, `cpt-cf-bss-rating-fr-commitment-drawdown`, `cpt-cf-bss-rating-fr-reservation-consumption-flavor`, `cpt-cf-bss-rating-fr-capacity-charge`, `cpt-cf-bss-rating-fr-coupon-application-order`, `cpt-cf-bss-rating-fr-coupon-stacking`, `cpt-cf-bss-rating-fr-multi-currency`, `cpt-cf-bss-rating-fr-fx-policy`, `cpt-cf-bss-rating-contract-finance-fx-input`, `cpt-cf-bss-rating-contract-promotions-coupon`, `cpt-cf-bss-rating-contract-contracts-input`.

**Principles**: `cpt-cf-bss-rating-principle-fail-closed`, `cpt-cf-bss-rating-principle-fixed-rule-order`, `cpt-cf-bss-rating-principle-stack-not-winner-ovl`, `cpt-cf-bss-rating-principle-total-order-ovl`, `cpt-cf-bss-rating-principle-surface-not-post-cmt`, `cpt-cf-bss-rating-principle-apply-never-own-cpn`, `cpt-cf-bss-rating-principle-no-implicit-fx`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-rating-actor-rating` | Calls the step evaluators through the core's step pipeline |
| `cpt-cf-bss-rating-actor-catalog` | Pricing: owns no overlays and no reserved rates on `main`; owns Promotions, which are deferred (D-409) |
| `cpt-cf-bss-rating-actor-partner-admin` | Prices a partner exception as another book or SKU in Pricing; nothing reaches this feature |
| `cpt-cf-bss-rating-actor-contracts` | Would supply contract overlays, commitment pools and negotiated reserved rates (no source at launch, R-11) |
| `cpt-cf-bss-rating-actor-promotions` | Would supply frozen coupon snapshots (no source at launch, R-11) |
| `cpt-cf-bss-rating-actor-finance-fx` | Would supply pinnable FX rate snapshots (no source at launch, R-07) |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6 (step 4 slots), §17.1.
- **Architecture**: [DESIGN.md](../DESIGN.md) §2.2 (`constraint-native-currency-launch`), §4.10 (capability matrix); namespaces [04](../DESIGN.md#contract-04) — [scope mapping](../DESIGN.md#contract-04-4-1), [stacking order](../DESIGN.md#contract-04-4-2), [contract overlay](../DESIGN.md#contract-04-4-3), [composition cap](../DESIGN.md#contract-04-4-4); [05](../DESIGN.md#contract-05) — [waterfall](../DESIGN.md#contract-05-4-1), [reservation flavors](../DESIGN.md#contract-05-4-2), [reserved-rate sources](../DESIGN.md#contract-05-4-3), [obligations](../DESIGN.md#contract-05-4-5); [06](../DESIGN.md#contract-06) — [placement](../DESIGN.md#contract-06-4-1), [stacking](../DESIGN.md#contract-06-4-2), [`applyScope`](../DESIGN.md#contract-06-4-3), [fail-closed snapshot](../DESIGN.md#contract-06-4-4); [07](../DESIGN.md#contract-07) — [currency roles](../DESIGN.md#contract-07-4-1), [FX policy](../DESIGN.md#contract-07-4-2), [ordering and precision](../DESIGN.md#contract-07-4-4); input contracts [11 §4.4](../DESIGN.md#contract-11-4-4), [11 §4.5](../DESIGN.md#contract-11-4-5), [11 §4.9](../DESIGN.md#contract-11-4-9). Runtime procedures of these namespaces are in [§7](#7-detailed-behavior-contracts).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.3.
- **Dependencies**: [Deterministic Evaluation Core](02-evaluation-core.md) (`cpt-cf-bss-rating-feature-evaluation-core`): step pipeline, exact arithmetic, error taxonomy, lineage and snapshot body.
- **Consumers**: [Child Evaluation](07-child-evaluation.md) and [Pre-Purchase Order Evaluation](10-order-evaluation.md) run the complete pipeline; [Corrections, Replay and Administrative Re-rate](08-corrections-rerate.md) applies the dormant pool-version cascade.
- **Upstream**: `cpt-cf-bss-rating-upreq-contracts-inputs`, `cpt-cf-bss-rating-upreq-promotions-coupon-snapshots` (Promotions are Pricing's, deferred D-409), `cpt-cf-bss-rating-upreq-fx-rate-snapshots` ([UPSTREAM_REQS](../UPSTREAM_REQS.md) §2.4, §2.9, §2.10); `cpt-cf-bss-rating-upreq-pricing-validator-hook` is closed (R-12).

**UI applicability**: none; the steps are in-process library code with no endpoint and no user interface.

## 2. Actor Flows (CDSL)

**Use cases**: `cpt-cf-bss-rating-usecase-partner-priceoverlay` — superseded on `main`: Pricing has no price overlays; a partner price is another book or SKU, which reaches Rating as an ordinary binding.

### 2.1 Apply the step-4 slots to one line

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-adjust-line`

**Actor**: `cpt-cf-bss-rating-actor-rating` (through the core's step pipeline)

**Success Scenarios**: with empty inputs the model amount passes the dormant steps unchanged and reaches the guards; each step records an empty segment.

**Error Scenarios**: the input names a source that does not exist (`reservation_match_unavailable`, `pool_input_torn`, `coupon_source_unavailable`, `fx_not_supported`).

**Steps**:
1. [ ] - `p1` - Price overlays (the former step 4): no input exists — `PriceOverlay` was removed from Pricing (§3.1) - `inst-al-step4`
2. [ ] - `p1` - Slot 4a: contract override; the contract input is empty (R-11) - `inst-al-step5`
3. [ ] - `p1` - The composition cap is checked only when contract overlays exist (§3.2) - `inst-al-cap`
4. [ ] - `p1` - Slot 4b: reservations and commitment pools (§3.3) - `inst-al-step6`
5. [ ] - `p1` - Slot 4c: coupons; the coupon input is empty (Promotions deferred, D-409) (§3.4) - `inst-al-step7`
6. [ ] - `p1` - Slot 4d: the binding's price currency against the subscription's billing currency; native only (§3.5) - `inst-al-step8`
7. [ ] - `p1` - **RETURN** the line; the minimum-fee floor (step 5, at roll-up) and the guards (step 6) of [Deterministic Evaluation Core](02-evaluation-core.md) follow - `inst-al-return`

The order is compiled in (PRD §17.1 as re-cut by T-D-73, [06 §4.1](../DESIGN.md#contract-06-4-1)); no configuration reorders it.

<a id="register-flows"></a>

### Migrated namespace flows

The flows of the former slice documents that this feature implements are defined here and specified in [§7](#7-detailed-behavior-contracts); each entry links to its contract.

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-stack-overlays-ovl`
  — Overlays — Interactions and Sequences ([contract](#contract-04-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-cap-clamp-ovl`
  — Overlays — Interactions and Sequences ([contract](#contract-04-3-6))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-step6-line-cmt`
  — Commitments — Interactions and Sequences ([contract](#contract-05-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-trueup-period-cmt`
  — Commitments — Interactions and Sequences ([contract](#contract-05-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-step7-line-cpn`
  — Coupons — Interactions and Sequences ([contract](#contract-06-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-billing-currency-pass-cpn`
  — Coupons — Interactions and Sequences ([contract](#contract-06-3-6))

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-step8-convert-fx`
  — Currency and FX — Interactions and Sequences ([contract](#contract-07-3-6))

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-close-delta-fx`
  — Currency and FX — Interactions and Sequences ([contract](#contract-07-3-6))

## 3. Processes / Business Logic (CDSL)

### 3.1 Stack price overlays

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-adjust-overlays`

**Input**: none on `main`.

**Output**: the line unchanged; an empty overlay segment.

1. [ ] - `p1` - Superseded (T-D-73): Pricing removed `PriceOverlay` (PriceBook spec item 2; legacy `pricing_price_overlay*` tables are refused at boot), so there is no overlay document, scope class or precedence to evaluate - `inst-ov-scope`
2. [ ] - `p1` - A partner or customer-group price is a different book or SKU; it arrives as an ordinary binding and is priced by the model step - `inst-ov-effective`
3. [ ] - `p1` - No input field can name a price overlay: neither the bindings nor the evaluation input carry one, so nothing is checked - `inst-ov-stack`
4. [ ] - `p1` - Customer-group-to-book mapping is "later, by separate decision" in Pricing; nothing is assumed - `inst-ov-invalid`
5. [ ] - `p1` - **RETURN** the line - `inst-ov-return`

### 3.2 Enforce the composition cap

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-adjust-cap`

**Input**: the model amount, the amount after contract overlays, their chain depth, `maxCumulativeMarkup` and its mode — all absent on `main`.

**Output**: the line unchanged, or a fail-closed error once contract overlays exist.

1. [ ] - `p1` - Dormant: with no price overlays and no contract overlays there is no chain to cap. When a Contracts source exists (R-11), a chain of depth ≥ 2 without a cap fails `composition_cap_missing`; never assume a default - `inst-cap-missing`
2. [ ] - `p1` - Then: a cumulative change over a `clamp` cap is clamped with the pre-clamp amount in lineage - `inst-cap-clamp`
3. [ ] - `p1` - Then: a change over a `hard` cap fails `composition_cap_exceeded` - `inst-cap-hard`
4. [ ] - `p1` - **RETURN** the line - `inst-cap-return`

`rating-val-01` has no input (no overlays); `rating-val-04` is inert until a Contracts source exists ([10 §4.2](../DESIGN.md#contract-10-4-2)).

### 3.3 Apply reservations and commitment pools

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-adjust-step6`

**Input**: the overlaid line, an optional `ReservationMatch`, the `CommitmentPoolInput` set, the `Step6Context` (remainder offset, capacity coverage).

**Output**: the line with reservation and pool effects in lineage, and any `TrueUpObligation`.

1. [ ] - `p1` - **IF** the input names a reservation and there is no match, return `reservation_match_unavailable` (no source: PriceBook bindings carry no reserved rate or `reservationFlavor`, and reserved capacity is "later, by separate decision" in Pricing; R-11) - `inst-s6-guard`
2. [ ] - `p1` - **IF** no reservation is named, pass the line through as pure usage pricing — the launch path - `inst-s6-none`
3. [ ] - `p1` - Dormant: apply the consumption or capacity flavor per [05 §4.2](../DESIGN.md#contract-05-4-2) with a reserved rate from the reserved-rate source of [05 §4.3](../DESIGN.md#contract-05-4-3) once one exists, or fail `reservation_rate_unresolved`; the consumption remainder re-runs the model step on the window-cumulative remainder axis - `inst-s6-flavor`
4. [ ] - `p2` - **IF** pools are present (dormant, R-11), draw them in declared order per [05 §4.1](../DESIGN.md#contract-05-4-1); **IF** a line claims pools that are absent or inconsistent, return `pool_input_torn` - `inst-s6-pools`
5. [ ] - `p2` - On a `committed_rate` pool's `period_line`, emit a `TrueUpObligation` for a non-zero shortfall ([05 §4.5](../DESIGN.md#contract-05-4-5)) - `inst-s6-trueup`
6. [ ] - `p1` - **RETURN** the line; the core never writes a balance and publishes no `CommitmentBalanceEffect` (dormant, [Child Evaluation](07-child-evaluation.md)) - `inst-s6-return`

### 3.4 Apply coupons

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-algo-adjust-coupons`

**Input**: the line after slot 4b (or after slot 4d for the billing-currency pass) and the frozen coupon snapshots in the input.

**Output**: the discounted line with discount lineage and the coupon snapshot segment.

1. [ ] - `p2` - **IF** the context names a coupon or promotion, return `coupon_source_unavailable`: Promotions are owned by Pricing and deferred (pricing D-409), resolve returns no promotion, and no binding carries a `discountRef` (R-11) - `inst-cp-guard`
2. [ ] - `p2` - Reject snapshots missing a required policy field per [06 §4.4](../DESIGN.md#contract-06-4-4) with the matching `coupon_*` error; never default a field - `inst-cp-policy`
3. [ ] - `p2` - Partition by `settlementCurrency`; apply the price-currency set at slot 4c and the billing-currency set after slot 4d ([06 §4.1](../DESIGN.md#contract-06-4-1)) - `inst-cp-partition`
4. [ ] - `p2` - Resolve stacking per [06 §4.2](../DESIGN.md#contract-06-4-2) and attach per `applyScope` per [06 §4.3](../DESIGN.md#contract-06-4-3); `line_total` fails `coupon_scope_unsupported` (T-D-22) - `inst-cp-stack`
5. [ ] - `p2` - **RETURN** the line; redemption state is never mutated - `inst-cp-return`

At launch no Promotions source exists, so the coupon input is always empty and no coupon segment is written.

### 3.5 Resolve currency and convert

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-adjust-fx`

**Input**: the binding's price currency (`ImmutablePrice.currency`, the book's single currency, pricing D-384), the subscription's billing currency, and an FX rate record when FX is enabled.

**Output**: the line in billing currency with the FX segment, or `fx_not_supported`.

1. [ ] - `p1` - Resolve `CurrencyRoles` per [07 §4.1](../DESIGN.md#contract-07-4-1): the price currency comes from the binding, the billing currency from the subscription; neither is re-derived - `inst-fx-roles`
2. [ ] - `p1` - **IF** billing currency equals price currency, skip conversion; the FX segment is empty - `inst-fx-native`
3. [ ] - `p1` - **ELSE** return `fx_not_supported` at launch (R-07); TARGET only: convert at full precision with the pinned rate record and record its `rate_ref` ([07 §4.2](../DESIGN.md#contract-07-4-2)) - `inst-fx-convert`
4. [ ] - `p1` - Run the billing-currency coupon pass, then return the line for the floor (step 5) and the guards (step 6) ([07 §4.4](../DESIGN.md#contract-07-4-4)) - `inst-fx-handoff`
5. [ ] - `p1` - **RETURN** the line without rounding - `inst-fx-return`

## 4. States (CDSL)

### 4.1 Step activation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-state-adjust-activation`

**States**: `empty_input` (launch: the step runs with no source data and fails closed when referenced), `active` (the step's source is delivered and its contract adopted).

**Initial State**: `empty_input` for slots 4a (contract override), 4b (pools and reservations), 4c (coupons) and 4d (conversion); price overlays have no state because their input no longer exists upstream.

**Transitions**:
1. [ ] - `p2` - **FROM** `empty_input` **TO** `active` **WHEN** `cpt-cf-bss-rating-upreq-contracts-inputs` is delivered (slot 4a contract override and slot 4b commitment pools; reservations additionally need a reservation-match source, R-11, and stay rejected until it exists) - `inst-act-contracts`
2. [ ] - `p2` - **FROM** `empty_input` **TO** `active` **WHEN** `cpt-cf-bss-rating-upreq-promotions-coupon-snapshots` is delivered (slot 4c) - `inst-act-promotions`
3. [ ] - `p2` - **FROM** `empty_input` **TO** `active` **WHEN** `cpt-cf-bss-rating-upreq-fx-rate-snapshots` is delivered (slot 4d conversion) - `inst-act-fx`

Activation that changes evaluation semantics is a new engine generation ([Deterministic Evaluation Core](02-evaluation-core.md)); the launch capability matrix (DESIGN §4.10) changes only when the owner decision is recorded.

## 5. Definitions of Done

### 5.1 Overlays and the composition cap

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-adjust-overlays`

The system **MUST** treat price overlays as superseded (no input exists or can be named), keep contract overlays and the composition cap dormant until a Contracts source exists — then apply them per namespace 04, with `clamp` and `hard` modes and `composition_cap_missing` (former `rating-val-03`) for an uncapped chain of depth ≥ 2 — and record applied ids in lineage only when any exist.

**Implements**: `cpt-cf-bss-rating-flow-adjust-line`, `cpt-cf-bss-rating-algo-adjust-overlays`, `cpt-cf-bss-rating-algo-adjust-cap`, `cpt-cf-bss-rating-flow-stack-overlays-ovl`, `cpt-cf-bss-rating-flow-cap-clamp-ovl`.

**Constraints**: `cpt-cf-bss-rating-constraint-publish-side-guarantees-ovl`, `cpt-cf-bss-rating-constraint-overlay-segment-ovl`, `cpt-cf-bss-rating-constraint-cap-modes-ovl`.

**Touches**: `cpt-cf-bss-rating-interface-stack-overlays-ovl`; entity `OverlaidLine`.

### 5.2 Reservations and commitments

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-adjust-step6`

The system **MUST** implement the slot-4b evaluator per namespace 05 as a dormant placeholder — both reservation flavors, the reserved-rate rule, the pool waterfall and true-up obligations activate only when Contracts (and a reserved-rate source) deliver inputs — run it with empty input at launch, and fail closed when an input names a reservation or pool that has no source.

**Implements**: `cpt-cf-bss-rating-algo-adjust-step6`, `cpt-cf-bss-rating-flow-step6-line-cmt`, `cpt-cf-bss-rating-flow-trueup-period-cmt`.

**Constraints**: `cpt-cf-bss-rating-constraint-fixed-slot-cmt`, `cpt-cf-bss-rating-constraint-reversal-boundary-cmt`, `cpt-cf-bss-rating-constraint-launch-posture-cmt`.

**Touches**: `cpt-cf-bss-rating-interface-step6-evaluator-cmt`, `cpt-cf-bss-rating-interface-contracts-input-cc`; entities `ReservationMatch`, `CommitmentPoolInput`.

### 5.3 Coupons

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-dod-adjust-coupons`

The system **MUST** implement the slot-4c evaluator and the billing-currency pass per namespace 06 — placement, stacking policies, `applyScope` attachment, fail-closed snapshot rules and discount lineage — and fail `coupon_source_unavailable` whenever a coupon is referenced while Promotions stay deferred (pricing D-409).

**Implements**: `cpt-cf-bss-rating-algo-adjust-coupons`, `cpt-cf-bss-rating-flow-step7-line-cpn`, `cpt-cf-bss-rating-flow-billing-currency-pass-cpn`.

**Constraints**: `cpt-cf-bss-rating-constraint-fixed-slot-cpn`, `cpt-cf-bss-rating-constraint-no-redemption-mutation-cpn`, `cpt-cf-bss-rating-constraint-promotions-maturity-cpn`.

**Touches**: `cpt-cf-bss-rating-interface-step7-evaluator-cpn`; entity `CouponSnapshot`.

### 5.4 Currency roles and FX

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-adjust-fx`

The system **MUST** resolve currency roles per namespace 07, rate only when billing currency equals the binding's price currency and otherwise fail `fx_not_supported`, keep the FX segment empty for native lines, and keep the TARGET conversion and invoice-period FX paths inactive until a pinnable rate source exists.

**Implements**: `cpt-cf-bss-rating-algo-adjust-fx`, `cpt-cf-bss-rating-state-adjust-activation`, `cpt-cf-bss-rating-flow-step8-convert-fx`, `cpt-cf-bss-rating-flow-close-delta-fx`.

**Constraints**: `cpt-cf-bss-rating-constraint-native-currency-launch`, `cpt-cf-bss-rating-constraint-finance-sor-fx`, `cpt-cf-bss-rating-constraint-binding-consumed-fx`, `cpt-cf-bss-rating-constraint-presentment-outside-fx`.

**Touches**: `cpt-cf-bss-rating-interface-convert-fx`; entity `CurrencyRoles`.

## 6. Acceptance Criteria

- [ ] No input field can name a price overlay; with empty contract input the model amount passes slot 4a unchanged.
- [ ] With a Contracts test fake: a contract stack exceeding a `clamp` cap is clamped with the pre-clamp amount in lineage; a `hard` cap fails `composition_cap_exceeded`.
- [ ] An input that names a reservation without a match fails `reservation_match_unavailable`; a line with none prices as pure usage.
- [ ] The capacity worked examples of 05 §4.2 return €14.40, €5.28 and €17.28 exactly when a match is supplied by a test fake.
- [ ] A context that names a coupon fails `coupon_source_unavailable`; a `line_total` coupon fails `coupon_scope_unsupported`.
- [ ] A line whose billing currency differs from the binding's price currency fails `fx_not_supported`; a native line has no FX segment.
- [ ] With empty contract, pool, coupon and FX inputs, a native-currency line leaves slots 4a–4d unchanged in amount; the snapshot body carries no overlay, coupon, commitment or FX segments (RECUT snapshot body).

Acceptance vectors owned by this feature (DESIGN §4.13 index):

| # | Scenario | Expected state |
|---|---|---|
| V27 | Dormant (R-11) — uncapped contract-overlay stack of depth ≥ 2 with no cap configured (Contracts test fake) | child fails `composition_cap_missing`; no line emitted |

## 7. Detailed Behavior Contracts

**Contract namespaces 04, 05, 06, 07.** Section numbers inside the detailed contracts below resolve through the [contract address index](../DECOMPOSITION.md#4-contract-address-index), not the overview section numbers above. A bare section reference names a section of the same namespace; "DESIGN §n" names §1–§5 of [DESIGN.md](../DESIGN.md).

The following sections keep the runtime procedures of these namespaces — their interactions and sequences and the procedural parts of their §4. The flows, processes and acceptance criteria above summarize them; the architecture, schemas and evaluation semantics they rely on are defined once in [DESIGN.md](../DESIGN.md) §6.

<a id="contract-04-3-6"></a>

<!-- contract:04-overlays-precedence:3.6 -->
### Overlays: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-stack-overlays-ovl` (`p1`), defined in [§2 migrated flows](#register-flows).

**Superseded** (T-D-73): Pricing removed `PriceOverlay`, so `ScopeFilter` and `OverlayStacker`
have no documents to read. The flow keeps one step: `ContractOverlayApplier` applies contract terms
once a Contracts source exists (none at launch, R-11), then `CompositionCapGuard` checks the
cumulative change; applied ids go to lineage; the line proceeds to slot 4b.

**Contract**: `cpt-cf-bss-rating-flow-cap-clamp-ovl` (`p2`), defined in [§2 migrated flows](#register-flows).

If the stack exceeds a `clamp` cap, the amount is set to the cap bound and the pre-clamp amount is
kept in lineage; under a `hard` cap the child fails with `composition_cap_exceeded`.

<!-- /contract -->

<a id="contract-05-3-6"></a>

<!-- contract:05-commitments-reservations:3.6 -->
### Commitments: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-step6-line-cmt` (`p1`), defined in [§2 migrated flows](#register-flows).

1. Guard: the input names a reservation and no match exists ⇒ `reservation_match_unavailable` (dormant, no source).
2. Capacity flavor: emit the capacity line (§4.2) on the `period_line` child.
3. Consumption flavor: split matched vs remainder; the remainder re-runs the model step (step 3) and slot 4a (T-D-13).
4. Waterfall over pools (when present); residual priced per the `overage_rate` selector (§4.1).
5. Record effects in lineage; continue to slot 4c.

**Contract**: `cpt-cf-bss-rating-flow-trueup-period-cmt` (`p2`), defined in [§2 migrated flows](#register-flows).

On the `period_line` child for a `committed_rate` pool: compute the shortfall (§4.5); emit a
`TrueUpObligation` in the outcome's `obligations`; zero shortfall emits nothing.

<!-- /contract -->

<a id="contract-06-3-6"></a>

<!-- contract:06-coupons:3.6 -->
### Coupons: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-step7-line-cpn` (`p2`), defined in [§2 migrated flows](#register-flows).

1. Guard: the context names a coupon and no Promotions source exists ⇒ `coupon_source_unavailable` (Promotions deferred, D-409).
2. Filter candidates; any missing policy field fails closed (§4.4).
3. Partition by `settlementCurrency`; apply the `price` set now.
4. Resolve stacking (§4.2) and attach per `applyScope` (§4.3).
5. Record lineage; continue to slot 4d.

**Contract**: `cpt-cf-bss-rating-flow-billing-currency-pass-cpn` (`p2`), defined in [§2 migrated flows](#register-flows).

After slot 4d, apply the `billing` set to the billing-currency amount with the same rules; then the
step-6 guards run. With native currency (launch) both passes operate on the same currency, in the
same fixed order.

<!-- /contract -->

<a id="contract-07-3-6"></a>

<!-- contract:07-currency-fx:3.6 -->
### Currency and FX: Interactions and Sequences

**Contract**: `cpt-cf-bss-rating-flow-step8-convert-fx` (`p1`), defined in [§2 migrated flows](#register-flows).

1. Resolve `CurrencyRoles` from the binding's price currency and the subscription's billing currency.
2. Equal ⇒ no conversion; FX segment empty.
3. Different ⇒ `fx_not_supported` (launch). Target: load the pinned `FxRateRecord`, convert at full
   precision, record `rate_ref`.
4. Apply billing-currency coupons ([contract 06](../DESIGN.md#contract-06)); hand back for steps 5 and 6.

**Contract**: `cpt-cf-bss-rating-flow-close-delta-fx` (`p2`), defined in [§2 migrated flows](#register-flows).

Target (R-07) — invoice-period FX: the child is evaluated with a provisional rate; once the
close-time rate record exists the child is re-evaluated with it, producing a new window revision and
a new parent revision like any other correction (DESIGN §3.6 Flow B). Finalization waits for the
close-time rate (pending reason `fx_rate_pending`), so Billing receives one complete revision rather
than a provisional one. Rating emits no delta (T-D-50): the rate record is a pinned rating input,
the "close" is the rate owner's publication of that record — Rating never reads Billing period
state, and Billing derives any monetary difference between revisions. The ID keeps its historical
name. Not active at launch.

<!-- /contract -->
