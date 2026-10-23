# Feature: Pre-Purchase Order Evaluation

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-featstatus-order-evaluation-implemented`

- [ ] `p1` - `cpt-cf-bss-rating-feature-order-evaluation`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Evaluate an order](#21-evaluate-an-order)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Validate an order evaluation request](#31-validate-an-order-evaluation-request)
  - [3.2 Compute totals and TCV](#32-compute-totals-and-tcv)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Order evaluation surface](#51-order-evaluation-surface)
  - [5.2 Exact totals from the shared arithmetic](#52-exact-totals-from-the-shared-arithmetic)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Serve `OrderEvaluationV1::evaluate` (Orders calls it `RatingPurchaseEvaluationV1`) in the shape Orders
selected (Orders D-197, D-216; T-D-83): given an order's lines — per line its Term, `BillingTerms` and,
per item, the selected Pricing binding and an explicit quantity — return per item, per line and for
the whole order the gross, discount and net amounts of the three charge kinds, the minimum-fee
application, currency and `currency_scale` (no rounding mode), the per-cycle recurring figures and one
net pre-tax TCV with its basis, computed by `rating-core`'s `evaluate_order` over exactly those
bindings, with nothing persisted and no `resolve` call.

### 1.2 Purpose

Orders needs totals at Preview, submit and amendment (Orders D-197; Orders UPSTREAM_REQS §2.2).
Serving them from the same arithmetic as fact rating, through a separate DTO that persists nothing
and is never a billing input, gives Orders exact numbers without making an order a rating input
(T-D-54). Rating owns model evaluation, minimum fees, quantity application, display rounding and every
aggregate; Orders checks identity, completeness and representability and stores the answer verbatim:
each amount as exact canonical decimal text and a display estimate (Orders D-216; money ADR rule 7).
Change orders' delta TCV is outside D-197 (Orders refuses `category = change`, D-176).

**Requirements** (primary, per DECOMPOSITION): `cpt-cf-bss-rating-fr-pre-purchase-evaluation`.

**Principles**: `cpt-cf-bss-rating-principle-pure-function-core`, `cpt-cf-bss-rating-principle-fail-closed`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-rating-actor-rating` | Serves the SDK call; the Orders service identity is its caller (not a PRD actor of this gear) |
| `cpt-cf-bss-rating-actor-catalog` | Pricing: the bindings in the request are the ones Pricing's assessment selected for the order (Orders D-189); a receipt issued later must carry the same bindings (Orders D-188); Rating reads no price itself |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6.1 (`fr-pre-purchase-evaluation`), §9.
- **Architecture**: [DESIGN.md](../DESIGN.md) [§3.3](../DESIGN.md#33-api-contracts) (`cpt-cf-bss-rating-interface-core-evaluate` — `evaluate_order`; `cpt-cf-bss-rating-interface-rating-client` — `OrderEvaluationV1`), [§4.8](../DESIGN.md#48-multi-tenancy-and-authorization) (operation matrix), [§4.10](../DESIGN.md#410-launch-scope) (capability matrix); [11 §4.10](../DESIGN.md#contract-11-4-10) (request, response and TCV rules — canonical).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.10.
- **Dependencies**: [Foundation](01-foundation.md) (authorization, SDK, pricing fake), [Evaluation Core](02-evaluation-core.md) (`evaluate_order`, money conversion T-D-74, floor T-D-78)
- **Consumers**: Orders (`gears/bss/orders/docs/DESIGN.md` `contract-03-rating-purchase-evaluation`; `gears/bss/orders/docs/UPSTREAM_REQS.md` §2.2).
- **Upstream**: `cpt-cf-bss-rating-upreq-orders-evaluation-dto` ([UPSTREAM_REQS §2.11](../UPSTREAM_REQS.md#211-orders-lifecycle)); decisions T-D-83 (Orders D-197, D-216) and R-24 (the delivery prerequisites Orders lists). The pricing adapter (R-17) is closed by T-D-73.

**UI applicability**: none; this is an in-process SDK contract. Actionable typed errors are its usability obligation.

## 2. Actor Flows (CDSL)

**Use cases**: none in the PRD; Orders' Preview, submit and amendment flows call this contract.

### 2.1 Evaluate an order

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-flow-order-evaluate`

**Actor**: Orders service identity, through `cpt-cf-bss-rating-actor-rating`'s SDK.

**Success Scenarios**: every line is evaluated over the bindings it carries; the response carries per item, line and order figures exact and as major-unit display estimates, the per-cycle recurring figures, one net pre-tax TCV with its basis, usage rates flagged and excluded, and the minimum-fee application; repeating the request returns the same result. On Preview a line without its term or cycle withholds only the TCV.

**Error Scenarios**: permission denied or an axis outside scope; prerequisites not in place (`ServiceUnavailable`); a typed error of the core (`adjustment_unsupported`, `fx_not_supported`, `binding_currency_mismatch`, `unsupported_primitive`, `period_geometry_mismatch`, `precision_overflow`, …), which Orders maps to `evaluation-unavailable`; never a partial answer or a guessed zero.

**Steps**:
1. [ ] - `p1` - Receive `OrderEvaluationV1::evaluate(ctx, OrderEvaluationRequest)` - `inst-oe-receive`
2. [ ] - `p1` - Authorize `evaluation × evaluate` under the Orders D-194 service grant, with the request's seller, resource and payer axes each in the PDP scope; the market is never authority - `inst-oe-authorize`
3. [ ] - `p1` - **IF** the capability is not enabled (the D-197 prerequisites are not in place, DESIGN §4.10), **RETURN** `ServiceUnavailable` - `inst-oe-gate`
4. [ ] - `p1` - Validate the request (§3.1) - `inst-oe-validate`
5. [ ] - `p1` - Take each item's price from the selected binding the request carries — no successor walk and no `resolve` call (Orders D-197); the request's bindings are Pricing's `AcceptedBinding`s, mapped into the core's frozen types (DESIGN [11 §4.10](../DESIGN.md#contract-11-4-10)) - `inst-oe-prices`
6. [ ] - `p1` - Call `evaluate_order` of the current engine generation (§3.2) - `inst-oe-evaluate`
7. [ ] - `p1` - **RETURN** `OrderEvaluation` with the echoed identity, input digest and evaluation policy; write nothing - `inst-oe-return`

## 3. Processes / Business Logic (CDSL)

### 3.1 Validate an order evaluation request

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-order-validate`

**Input**: the D-197 request ([11 §4.10](../DESIGN.md#contract-11-4-10)): identity, schema and profile version, input digest, axes, market, contract reference, and per line its Term, `BillingTerms` and per item the selected binding and quantity.

**Output**: an accepted request or a typed refusal.

1. [ ] - `p1` - **IF** a submit or amendment line lacks its term or cycle, or names an unknown schema or profile, refuse; a Preview line may carry the tagged missing term or cycle (Orders D-189, D-193), which withholds the TCV - `inst-ov-term`
2. [ ] - `p1` - **IF** an item has no selected binding or a line names a promotion, refuse; never substitute a default. The core refuses a model its charge kind does not take (pricing D-504) and a contract reference - `inst-ov-unsupported`
3. [ ] - `p1` - **IF** a binding's currency differs from the market currency, the core fails `fx_not_supported`; never convert (native currency at launch, R-07) - `inst-ov-currency`
4. [ ] - `p1` - **RETURN** the accepted request - `inst-ov-return`

### 3.2 Compute totals and TCV

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-algo-order-tcv`

**Input**: the accepted request, as `evaluate_order`'s `OrderEvaluationInput`.

**Output**: `OrderEvaluation{assessment_id, currency, currency_scale, lines[{line_id, items[{item_id, price_id, charge_kind, cycle?, rated?, minimum_fee?, minimum_fee_applied, gross?, discount_status, net?, usage_rates?}], rows, horizon?}], order: rows, tcv?{amount, basis, evidence}, tcv_withheld, engine}` with `rows = [{charge_kind, amount_status, gross?, discount_status, net?, amount_basis?, recurring_by_cycle}]` (DESIGN [11 §4.10](../DESIGN.md#contract-11-4-10)); a recurring item's amounts are per `cycle`, which must equal the binding's `recurring_period` (else `period_geometry_mismatch`).

1. [ ] - `p1` - Per item compute recurring (per cycle) and one-time `{gross, net}` exactly, each floored at its `minimum_fee` (covered fraction 1, T-D-78); the quantity applies to `per_unit` only; `discount_status` is `unavailable`, never a zero discount, and net equals gross (promotions are deferred in Pricing, D-409). Per line, the recurring aggregate is the per-cycle sum times the periods of its term — `count`, 12 for a rolling `month`, 1 for a rolling `year` — on the basis `finite_term` or `rolling_annualized`; per order, the sum of the lines', on the basis the lines with recurring amounts share or `mixed` (an order without recurring amounts takes its lines' terms); TCV = the order's recurring aggregate plus its one-time amounts, net, pre-tax, carried on the order's `recurring` row - `inst-tcv-term`
2. [ ] - `p1` - Report usage as rates (the binding's model, unit and `usage_rating_policy` window) with `amount_status = uncommitted_usage` and no amount; report each usage price's `minimum_fee` without applying it (consumption is unknown); never predict consumption - `inst-tcv-usage`
3. [ ] - `p1` - Return every amount as `{exact, display}`: `exact` is canonical decimal text in major units (at most 28 significant and 28 fractional digits, Orders D-216, else `precision_overflow`) and `display = round_half_even(exact, currency_scale)` in canonical text — each display rounded from its own exact value; the response is a nonbinding estimate (money ADR rule 7) and carries no tax - `inst-tcv-tax`
4. [ ] - `p1` - Echo the request's input digest and **RETURN** - `inst-tcv-return`

## 4. States (CDSL)

None: an order evaluation persists nothing and has no lifecycle (T-D-54).

## 5. Definitions of Done

### 5.1 Order evaluation surface

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-order-surface`

The system **MUST** expose `OrderEvaluationV1::evaluate` through ClientHub with `evaluation × evaluate` authorization under the Orders D-194 service grant, refuse with `ServiceUnavailable` until the D-197 prerequisites hold, validate requests fail-closed, and persist nothing: no idempotency key, no reservation, no row.

**Implements**: `cpt-cf-bss-rating-flow-order-evaluate`, `cpt-cf-bss-rating-algo-order-validate`.

**Constraints**: `cpt-cf-bss-rating-constraint-inprocess-boundary-cc`, `cpt-cf-bss-rating-constraint-domain-boundaries`.

**Touches**: `cpt-cf-bss-rating-interface-rating-client` (`OrderEvaluationV1`), `cpt-cf-bss-rating-interface-order-evaluation-cc`; no table.

### 5.2 Exact totals from the shared arithmetic

- [ ] `p1` - **ID**: `cpt-cf-bss-rating-dod-order-totals`

The system **MUST** compute every amount with `rating-core`'s `evaluate_order` over the request's bindings and the same exact arithmetic as fact rating, return each amount as exact and display decimal text (HALF_EVEN at `currency_scale`), apply the horizon and TCV rules of [11 §4.10](../DESIGN.md#contract-11-4-10), and exclude usage from TCV.

**Implements**: `cpt-cf-bss-rating-algo-order-tcv`.

**Constraints**: `cpt-cf-bss-rating-constraint-exact-no-rounding`.

**Touches**: `cpt-cf-bss-rating-interface-core-evaluate` (`evaluate_order`); no pricing read (the bindings arrive in the request).

Observability: `rating_order_evaluation_seconds` (DESIGN §3.2 component table).

## 6. Acceptance Criteria

- [ ] A rolling `month` line returns TCV = recurring net × 12 + one-time net on the basis `rolling_annualized`; a `year` line × 1; a `fixed_periods{count}` line count × recurring + one-time on the basis `finite_term`; an order of both is `mixed` — exact, each with `display` = the HALF_EVEN rounding of its exact value at `currency_scale`.
- [ ] Per-cycle recurring figures stay in `recurring_by_cycle`, one entry per cycle, never summed across cycles.
- [ ] A usage item returns its rates, window and `minimum_fee`, has no amount (`uncommitted_usage`) and contributes nothing to TCV; a usage-only order still carries its `recurring` row with TCV 0.
- [ ] A recurring item rated below its `minimum_fee` is floored per cycle and counted in TCV at the floor; a one-time item likewise, once.
- [ ] `discount_status` is `unavailable`, never a zero discount (pricing D-409).
- [ ] A Preview line without its term or cycle withholds the TCV and is listed in `tcv_withheld`; every other figure is returned.
- [ ] A contract reference, a binding in another currency or at another scale, a model outside the sale profile, a recurring period other than the line's cycle or a term of zero periods returns its typed error; nothing is returned in part.
- [ ] The evaluation calls no `resolve`: an item is priced at exactly the binding the request carries (Orders D-197).
- [ ] A caller without `evaluation × evaluate`, or with an axis outside its scope, is refused.
- [ ] Until the D-197 prerequisites hold (R-24) the call returns `ServiceUnavailable`.
- [ ] Two identical requests with unchanged pricing return identical responses; no row is written by either.
- [ ] An amount that is not decimal text of at most 28 significant and 28 fractional digits fails `precision_overflow`; it is never rounded.
- [ ] **V48** `PerUnit{unit_amount: "0.333"}` × 1 on a rolling `month` line: exact 0.333, display 0.33; TCV exact 3.996, display 4 (canonical text, Orders D-216), basis `rolling_annualized`; the response carries no integer minor units and no tax; Billing's postings are authoritative (**Rating**; T-D-80, T-D-83).
