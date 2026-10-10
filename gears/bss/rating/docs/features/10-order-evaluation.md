# Feature: Pre-Purchase Order Evaluation

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-featstatus-order-evaluation-implemented`

- [ ] `p2` - `cpt-cf-bss-rating-feature-order-evaluation`

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

Serve `OrderEvaluationV1::evaluate` in the shape Orders Lifecycle specifies (Orders D-154, D-167;
T-D-79): given an order's lines with the exact bindings Orders' assessment accepted — per line the
`plan_revision_id`, its `term` and `cycle`, per item the `quantity` and its chains `{dim_value, binding}` — return per item,
per line and for the whole order the gross, discount and net amounts of the three charge kinds,
the minimum-fee application, currency, `currency_scale` and rounding, and the net pre-tax TCV,
computed by `rating-core`'s `evaluate_order` over exactly those bindings, with nothing persisted and
no `resolve` call.

### 1.2 Purpose

Orders Lifecycle needs totals at submit and amendment, and change orders need delta totals
(UPSTREAM_REQS G-6). Serving them from the same arithmetic as fact rating, through a separate DTO that
persists nothing and is never a billing input, gives Orders exact numbers without making an order a
rating input (T-D-54). Orders does not consume the Atlas C06 DTO (`acceptance_ref`, `ExactAmount`
only) and stores a display total, so the response carries both forms (T-D-79): the exact amount and
a major-unit display decimal marked as a non-authoritative estimate (money ADR rule 7; T-D-80).
Orders' D-40 and D-167 documents still name integer minor units, which the money ADR retires; Rating
returns none (R-24).

**Requirements** (primary, per DECOMPOSITION): `cpt-cf-bss-rating-fr-pre-purchase-evaluation`.

**Principles**: `cpt-cf-bss-rating-principle-pure-function-core`, `cpt-cf-bss-rating-principle-fail-closed`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-bss-rating-actor-rating` | Serves the SDK call; the Orders Lifecycle service identity is its caller (not a PRD actor of this gear) |
| `cpt-cf-bss-rating-actor-catalog` | Pricing: the bindings in the request are resolve bindings Orders obtained (`plan_revision_id` and `resolve_date` of its assessment); Rating reads no price itself |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md) §6.1 (`fr-pre-purchase-evaluation`), §9.
- **Architecture**: [DESIGN.md](../DESIGN.md) [§3.3](../DESIGN.md#33-api-contracts) (`cpt-cf-bss-rating-interface-core-evaluate` — `evaluate_order`; `cpt-cf-bss-rating-interface-rating-client` — `OrderEvaluationV1`), [§4.8](../DESIGN.md#48-multi-tenancy-and-authorization) (operation matrix), [§4.10](../DESIGN.md#410-launch-scope) (capability matrix); [11 §4.10](../DESIGN.md#contract-11-4-10) (request, response and TCV rules — canonical).
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md) §2.10.
- **Dependencies**: [Foundation](01-foundation.md) (authorization, SDK, pricing fake), [Evaluation Core](02-evaluation-core.md) (`evaluate_order`, money conversion T-D-74, floor T-D-78), [Price Adjustments](03-price-adjustments.md) (fail-closed dormant steps).
- **Consumers**: Orders Lifecycle (`gears/bss/orders-lifecycle/docs/UPSTREAM_REQS.md` §2.2), change orders.
- **Upstream**: `cpt-cf-bss-rating-upreq-orders-evaluation-dto` ([UPSTREAM_REQS §2.11](../UPSTREAM_REQS.md#211-orders-lifecycle)); decision R-24 (Orders confirms the T-D-79 contract and the priority). The pricing adapter (R-17) is closed by T-D-73.

**UI applicability**: none; this is an in-process SDK contract. Actionable `InvalidArgument` errors are its usability obligation.

## 2. Actor Flows (CDSL)

**Use cases**: none in the PRD; Orders Lifecycle's submit and amendment flows call this contract.

### 2.1 Evaluate an order

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-flow-order-evaluate`

**Actor**: Orders Lifecycle service identity, through `cpt-cf-bss-rating-actor-rating`'s SDK.

**Success Scenarios**: every line is evaluated over the bindings it carries; the response carries per item, line and order figures exact and as major-unit display decimals marked as estimates, the net pre-tax TCV, usage rates flagged and excluded, and the minimum-fee application; repeating the request returns the same result.

**Error Scenarios**: permission denied or seller tenant outside scope; missing term or cycle, a cycle other than `month` or `year`, or a chain without a binding (`InvalidArgument`); a promotion or an unsupported model (explicit error, never a default); prerequisites not in place (`ServiceUnavailable`); any failure Orders maps to `evaluation-unavailable`; `precision_overflow`.

**Steps**:
1. [ ] - `p1` - Receive `OrderEvaluationV1::evaluate(ctx, OrderEvaluationRequest)` - `inst-oe-receive`
2. [ ] - `p1` - Authorize `evaluation × evaluate` for the request's seller tenant, which must lie in the PDP scope - `inst-oe-authorize`
3. [ ] - `p1` - **IF** the capability is not enabled (R-24 not confirmed by Orders, DESIGN §4.10), **RETURN** `ServiceUnavailable` - `inst-oe-gate`
4. [ ] - `p1` - Validate the request (§3.1) - `inst-oe-validate`
5. [ ] - `p1` - Take each item's price from the binding of its selected chain in the request — exact-price mode: no renewal walk and no `resolve` call (Orders D-167); the request's bindings are Pricing's `AcceptedBinding`s as Orders stored them (DESIGN [11 §4.10](../DESIGN.md#contract-11-4-10)) - `inst-oe-prices`
6. [ ] - `p1` - Call `evaluate_order` of the current engine generation and apply the TCV rules (§3.2) - `inst-oe-evaluate`
7. [ ] - `p1` - **RETURN** `OrderEvaluation`; write nothing - `inst-oe-return`

## 3. Processes / Business Logic (CDSL)

### 3.1 Validate an order evaluation request

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-algo-order-validate`

**Input**: `OrderEvaluationRequest{assessment_id, resolve_date, tenant_axes, market{currency, region}, contract_ref?, lines[{line_id, plan_revision_id, term: Rolling | FixedPeriods{count}, cycle: month | year, items[{item_id, quantity, chains[{dim_value, binding}]}]}]}` ([11 §4.10](../DESIGN.md#contract-11-4-10)); `market`, `contract_ref` and the per-line `term`/`cycle` are Rating's additions pending Orders' confirmation (R-24) of the context Orders lists (`orders-lifecycle/docs/UPSTREAM_REQS.md:306-309`).

**Output**: an accepted request or a typed refusal.

1. [ ] - `p1` - **IF** a line lacks its term or cycle, or its recurring period is not `month` or `year`, refuse `InvalidArgument` - `inst-ov-term`
2. [ ] - `p1` - **IF** an item's selected chain has no binding, or a line names a promotion or a model its charge kind does not allow (pricing D-386), refuse with the explicit error; never substitute a default - `inst-ov-unsupported`
3. [ ] - `p1` - **IF** a line's price currency differs from the request currency, fail `fx_not_supported`; never convert (native currency at launch, R-07) - `inst-ov-currency`
4. [ ] - `p1` - **RETURN** the accepted request - `inst-ov-return`

### 3.2 Compute totals and TCV

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-algo-order-tcv`

**Input**: the per-line `evaluate_order` outcome.

**Output**: `OrderEvaluation{assessment_id, estimate: true, currency, currency_scale, rounding: HALF_EVEN, lines[{line_id, items[{item_id, charge_kind, gross, discount, net, minimum_fee_applied?, usage_flagged, usage_rates?}], totals}], order_totals, tcv, promotion_status}` (DESIGN [11 §4.10](../DESIGN.md#contract-11-4-10)); a recurring item's amounts are per `cycle`, which must equal the bindings' `recurring_period` (else `InvalidArgument`).

1. [ ] - `p1` - Per item and line compute recurring and one-time `{gross, discount, net}` exactly (`discount` and `promotion_status` are `unavailable`, never a zero discount: promotions are deferred in Pricing, D-409); apply the term rules of [11 §4.10](../DESIGN.md#contract-11-4-10): rolling terms annualised ×12 for `month` and ×1 for `year`, a finite term counted per period, one-time counted once; the net pre-tax TCV excludes usage - `inst-tcv-term`
2. [ ] - `p1` - Report usage as rates (the binding's money, its `usage_rating_policy` window) and flag it excluded; report each usage price's `minimum_fee` as its minimum-fee application per period (T-D-78); never predict consumption - `inst-tcv-usage`
3. [ ] - `p1` - Return every amount twice: exact (`ExactAmount`, major units) and as a display decimal in major units rounded HALF_EVEN at the binding's `currency_scale`, each per-cycle and one-time figure rounded from its own exact value and the display TCV built from those rounded per-cycle figures (sum over items of `periods × round(per-cycle net)` + rounded one-time), the exact TCV returned beside it, with the response marked `estimate: true` — the order evaluation is not authoritative, so this rounding never reaches Billing (T-D-79, T-D-80; money ADR rule 7); set `tax_included = false`; report `discount` and `promotion_status` as `unavailable` - `inst-tcv-tax`
4. [ ] - `p1` - Compute `request_digest` over the canonical request and **RETURN** - `inst-tcv-return`

## 4. States (CDSL)

None: an order evaluation persists nothing and has no lifecycle (T-D-54).

## 5. Definitions of Done

### 5.1 Order evaluation surface

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-dod-order-surface`

The system **MUST** expose `OrderEvaluationV1::evaluate` through ClientHub with `evaluation × evaluate` authorization, refuse with `ServiceUnavailable` until its launch prerequisites hold, validate requests fail-closed, and persist nothing: no idempotency key, no reservation, no row.

**Implements**: `cpt-cf-bss-rating-flow-order-evaluate`, `cpt-cf-bss-rating-algo-order-validate`.

**Constraints**: `cpt-cf-bss-rating-constraint-inprocess-boundary-cc`, `cpt-cf-bss-rating-constraint-domain-boundaries`.

**Touches**: `cpt-cf-bss-rating-interface-rating-client` (`OrderEvaluationV1`), `cpt-cf-bss-rating-interface-order-evaluation-cc`; no table.

### 5.2 Exact totals from the shared arithmetic

- [ ] `p2` - **ID**: `cpt-cf-bss-rating-dod-order-totals`

The system **MUST** compute every amount with `rating-core`'s `evaluate_order` over the request's bindings and the same exact arithmetic as fact rating, return each amount exact and as a major-unit display decimal (HALF_EVEN at `currency_scale`, marked as an estimate), apply the TCV rules of [11 §4.10](../DESIGN.md#contract-11-4-10), and exclude usage from TCV.

**Implements**: `cpt-cf-bss-rating-algo-order-tcv`.

**Constraints**: `cpt-cf-bss-rating-constraint-exact-no-rounding`.

**Touches**: `cpt-cf-bss-rating-interface-core-evaluate` (`evaluate_order`); no pricing read (the bindings arrive in the request).

Observability: `rating_order_evaluation_seconds` (DESIGN §3.2 component table).

## 6. Acceptance Criteria

- [ ] A rolling `month` line returns TCV = recurring net × 12 + one-time net; a `year` line × 1; a finite line periods × recurring + one-time — exact, with a display TCV of `periods × round(per-cycle net)` + rounded one-time net, HALF_EVEN at `currency_scale`.
- [ ] A usage line returns its rates, window and `minimum_fee` with usage flagged excluded and contributes nothing to TCV.
- [ ] `discount` and `promotion_status` are returned as `unavailable`, never as a zero discount (pricing D-409).
- [ ] A line without term or cycle, or with a cycle other than `month` or `year`, returns `InvalidArgument`; a chain without a binding, a promotion or a model its charge kind does not allow returns its explicit error.
- [ ] The evaluation calls no `resolve`: an item is priced at exactly the binding the request carries (Orders D-167).
- [ ] A caller without `evaluation × evaluate`, or with a seller tenant outside its scope, is refused.
- [ ] Until Orders confirms the T-D-79 contract (R-24) the call returns `ServiceUnavailable`.
- [ ] Two identical requests with unchanged pricing return identical responses and `request_digest`; no row is written by either.
- [ ] An amount beyond the `ExactAmount` bound fails `precision_overflow`; it is never rounded.
- [ ] **V48** `PerUnit{unit_amount: "0.333"}` × 1 on a rolling `month` line: exact 0.333, display 0.33; TCV exact 3.996, display 3.96 (12 × the 0.33 Billing posts a month, not 4.00); the response is marked `estimate: true` and carries no integer minor units; Billing's postings are authoritative (**Rating**; T-D-80).
