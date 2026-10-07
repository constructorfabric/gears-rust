<!-- CONFLUENCE_TITLE: [BSS]: Pricing — Read Contract & Events (Feature) -->
<!-- Related: ../DECOMPOSITION.md, ../DESIGN.md, ../PRD.md | Owners: BSS Pricing team -->

# Feature: Read Contract & Events

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-featstatus-read-contract-events-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p1` - `cpt-cf-bss-pricing-feature-read-contract-events`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Resolve a renewal and preserve invoice inputs](#resolve-a-renewal-and-preserve-invoice-inputs)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [renewal-walk](#renewal-walk)
  - [period-slices-and-quote](#period-slices-and-quote)
  - [typed-events](#typed-events)
- [4. States (CDSL)](#4-states-cdsl)
  - [Read Contract & Events states](#read-contract--events-states)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Full chain resolution matrix](#full-chain-resolution-matrix)
  - [Renewal walk and eligibility](#renewal-walk-and-eligibility)
  - [Descriptors from the dated SKU version](#descriptors-from-the-dated-sku-version)
  - [Durable pinned-price read](#durable-pinned-price-read)
  - [Period boundary semantics](#period-boundary-semantics)
  - [Studio quote calculation](#studio-quote-calculation)
  - [Typed transactional domain events](#typed-transactional-domain-events)
  - [Frozen consumer golden responses](#frozen-consumer-golden-responses)
- [6. Acceptance Criteria](#6-acceptance-criteria)
  - [Durable accepted terms (D-507)](#durable-accepted-terms-d-507)
  - [Frozen fulfilment (D-508)](#frozen-fulfilment-d-508)
  - [Executable Pricing consumer fixtures (D-509)](#executable-pricing-consumer-fixtures-d-509)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

**Delivery:** phase 4 for reads, with quote not built (D-415); 2c for core events; 3 for added events. Every checkbox is an implementation obligation, not an assertion about the legacy code. Core event payloads are built with phase 2 approvals; dependencies on plans/promotions apply only to their later reads and events.

This feature implements [slice 07](../design/07-read-contract-events.md).
[DECOMPOSITION](../DECOMPOSITION.md) records integration order; [DESIGN §3](../DESIGN.md#3-technical-architecture)
is the schema and transaction authority. Unchecked phase 3/4 work is not part of the phase 2 core gate.

### 1.2 Purpose

Deliver reproducible resolution matrices, pinned-price reads and Studio quote, plus typed transactional events and consumer goldens. The Studio quote is not built (D-415).

Requirements: `cpt-cf-bss-pricing-fr-resolve`, `cpt-cf-bss-pricing-fr-price-read`, `cpt-cf-bss-pricing-fr-quote`, `cpt-cf-bss-pricing-fr-events`.

Architecture: `cpt-cf-bss-pricing-component-read-contract`, `cpt-cf-bss-pricing-component-events`, `cpt-cf-bss-pricing-component-prices`, `cpt-cf-bss-pricing-principle-book-money-independent`, `cpt-cf-bss-pricing-constraint-two-backends`.

### 1.3 Actors

`cpt-cf-bss-pricing-actor-rating`, `cpt-cf-bss-pricing-actor-subscriptions`, `cpt-cf-bss-pricing-actor-products`, `cpt-cf-bss-pricing-actor-finance-manager`. Every operation authenticates and derives tenant scope before storage, replay or cross-gear calls.
Holding multiple permissions never bypasses separation of duties.

### 1.4 References

- [PRD](../PRD.md), especially the numbered acceptance criteria referenced below.
- [DESIGN](../DESIGN.md), §3 model, API contracts, transaction sequences and DDL.
- [Slice 07](../design/07-read-contract-events.md), including API, data and event obligations.
- [DECISIONS](../DECISIONS.md), D-384–D-433; spec means `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout.
- Source: spec §2 decisions 4–8, 13–17, §2.2, §5–§8, §10, §12–§13; the phase 2 plan supplies delivery boundaries and D-399/D-400.

D-503 projects the entry's optional typed `usage_rating_policy` on each REST resolve item
and each SDK binding. The materialized identity/content is loaded from local policy storage alongside
the selected entry; historical reads never call the meter provider. SDK bindings retain the same
`price_book_entry_id` as their price. Entry reads and exports retain D-502's optional projection.
A BillingCycle VM entry beside a CalendarHour cloudlet entry keeps two independent policies;
there is no plan-wide window or aggregation across subscription lines. Missing legacy policy is null.

## 2. Actor Flows (CDSL)

### Resolve a renewal and preserve invoice inputs

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-flow-read-contract-events`

1. [x] - `p1` - Rating or Subscriptions sends the revision id, period start and optional current pins. - `inst-read-contract-events-flow-1`
2. [x] - `p1` - Load immutable revision structure and the full chain matrix, scoped to the tenant. - `inst-read-contract-events-flow-2`
3. [x] - `p1` - For existing pins walk eligible all successors, stopping before the first new price; for signup select in-force prices. - `inst-read-contract-events-flow-3`
4. [x] - `p1` - Read Products versions?as_of for the period start, bind descriptors/timing/meter/unit with their source (entry, SKU or tenant, D-421) and return the whole dimension matrix. - `inst-read-contract-events-flow-4`
5. [x] - `p1` - A later replay reads the pinned price by id (GET /bss-pricing/v1/prices/{id}, D-422): the approved money is served forever, whatever its window; binding usage lazily per value and keeping the complete inputs are the consumer's part. - `inst-read-contract-events-flow-5`
6. [ ] - `p1` - Return the active promotion (id, version) with the matrix: deferred with promotions (D-409), this step stays unticked until they return. - `inst-read-contract-events-flow-6`

## 3. Processes / Business Logic (CDSL)

### renewal-walk

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-algo-read-contract-events-renewal-walk`

1. [x] - `p1` - Validate the supplied pin belongs to the tenant, revision item and entry chain. - `inst-read-contract-events-renewal-walk-1`
2. [x] - `p1` - From the pinned price walk successors with eligibility all, bounded by the relevant date; stop before the first new successor. - `inst-read-contract-events-renewal-walk-2`
3. [x] - `p1` - Without a pin choose the in-force price per value, falling back to default where the value has no active price. - `inst-read-contract-events-renewal-walk-3`
4. [x] - `p1` - Return uncovered rather than inventing a price when neither chain covers; preserve historical keep_for_bound prices. - `inst-read-contract-events-renewal-walk-4`

### period-slices-and-quote

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-read-contract-events-period-slices-and-quote`

Not built (D-415): the owner dropped quote and the Studio wiring; consumers read resolve and GET /bss-pricing/v1/prices/{id}, and Rating owns the minimum-fee floor arithmetic.

1. [ ] - `p1` - Split the period at every price boundary inside the bound chain, including a temporary end. - `inst-read-contract-events-period-slices-and-quote-1`
2. [ ] - `p1` - Prorate recurring slices by calendar days; rate usage by reading timestamp with tier counters per slice. - `inst-read-contract-events-period-slices-and-quote-2`
3. [ ] - `p1` - Aggregate per price/subscription/period (no plan carries an included quantity to deduct since D-467) and apply the coverage-prorated min_fee once per price; not built in pricing, Rating applies the floor (D-415). - `inst-read-contract-events-period-slices-and-quote-3`
4. [ ] - `p1` - Apply period-start promotion after floors, then the bound rounding/currency policy; quote returns totals while resolve never does; quote is not built (D-415). - `inst-read-contract-events-period-slices-and-quote-4`

### typed-events

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-algo-read-contract-events-typed-events`

1. [x] - `p1` - Implement PricesPublished, ApprovalUnitDecided and PriceBookEntryReferenceLost through broker TypedEvent in phase 2. - `inst-read-contract-events-typed-events-1`
2. [x] - `p1` - Add PlanRevisionPublished, PlanReferenceLost, PlanRetired, PromotionPublished and SubscriptionMigrationRequested as their phase 3 acts become real; PromotionPublished (D-409), PlanRetired and SubscriptionMigrationRequested (D-410) are deferred. - `inst-read-contract-events-typed-events-2`
3. [x] - `p1` - Append event and audit through the same mutation transaction; encode the tenant and stable subject identities in the durable envelope, the correlation id staying on the audit rows of the same transaction. - `inst-read-contract-events-typed-events-3`
4. [ ] - `p1` - Deliver from the toolkit dispatcher after commit; test restart/retry and prevent domain publish on reject/withdraw/refresh. - `inst-read-contract-events-typed-events-4`

## 4. States (CDSL)

### Read Contract & Events states

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-state-read-contract-events`

A consumer binding is created for a period and stays immutable for replay. New all prices affect later renewal binding; a new price blocks forward renewal walking until explicit migration. Closed/superseded/keep_for_bound prices remain readable. Event states belong to toolkit delivery; Pricing does not maintain a second outbound state machine.

## 5. Definitions of Done

Every DoD below is required for this feature's delivery phase. Constraints: `cpt-cf-bss-pricing-constraint-two-backends`.

### Full chain resolution matrix

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-resolve-matrix`

Resolve returns every item's default/value inputs and active promotion version (deferred with promotions, D-409) without totals. Usage consumers bind lazily by value rather than choosing one value for the plan (spec §7.1).

Requirement: `cpt-cf-bss-pricing-fr-resolve`; PRD AC #18.

### Renewal walk and eligibility

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-renewal-all-new`

Existing pins advance through all successors and stop before the first new price. Signup chooses the in-force price and keep_for_bound preserves the predecessor needed by renewals (spec §7.1, §12).

Requirement: `cpt-cf-bss-pricing-fr-resolve`; PRD AC #18.

### Descriptors from the dated SKU version

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-binding-sku-version`

Binding reads versions?as_of at period start, not the latest mutable SKU, as pricing's system actor on both REST resolve and `PricingReadV1::resolve` after the caller passed plan read (D-424). Preserve version, unit/meter, descriptors, timing, rounding and currency scale in replay inputs (spec §2.2, §7.1). An SDK binding that cannot supply a commercial input is `INCOMPLETE_COMMERCIAL_INPUTS` on that field (D-501). REST keeps the descriptor nullable.

Requirement: `cpt-cf-bss-pricing-fr-resolve`; PRD AC #18.

### Durable pinned-price read

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-price-read-forever`

The public price-id read serves original approved money after closure, supersession or keep_for_bound. A cancelled price is served with its money as approved and says so: `status: cancelled` on REST, `state` `Cancelled` in the SDK (D-520). Tenant scope remains enforced and no retention deletes a pinned fact (spec §7.1).

Requirement: `cpt-cf-bss-pricing-fr-price-read`; PRD AC #19.

### Period boundary semantics

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-period-slices`

Not built (D-415): the owner dropped quote and the Studio wiring; consumers read resolve and GET /bss-pricing/v1/prices/{id}, and Rating owns the minimum-fee floor arithmetic. This DoD stays unticked.

Recurring slices prorate by calendar days and usage follows timestamp-selected prices, with counters per slice. Temporary boundaries inside a period produce multiple slices and price-level floor aggregation (spec §7.1).

Requirement: `cpt-cf-bss-pricing-fr-quote`; PRD AC #20.

### Studio quote calculation

- [ ] `p1` - **ID**: `cpt-cf-bss-pricing-dod-quote-totals`

Not built (D-415): the owner dropped quote and the Studio wiring; consumers read resolve and GET /bss-pricing/v1/prices/{id}, and Rating owns the minimum-fee floor arithmetic. This DoD stays unticked.

Quote adds quantities to selection, price floors and promotions (no optional item and no included quantity since D-467). It is read-only and separate from resolve, with exact tier-edge and rounding goldens (spec §7.1).

Requirement: `cpt-cf-bss-pricing-fr-quote`; PRD AC #20.

### Typed transactional domain events

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-events-typed-outbox`

Core domain events are TypedEvents enqueued on the toolkit outbox in the mutation transaction and delivered by the broker SDK producer when an EventBrokerApi is registered (D-400); later payloads take the same shape. Envelope and payload identity are preserved. Terminal units always event; submission and committed refresh do not falsely publish domain success (spec §6–§7.3, D-400).

Requirement: `cpt-cf-bss-pricing-fr-events`; PRD AC #14.

### Frozen consumer golden responses

- [x] `p1` - **ID**: `cpt-cf-bss-pricing-dod-consumer-golden-contracts`

D-501 adds real SDK provider tests for explicit catalog/PDP authorization, typed complete matrices,
permanent approved money and scheduled-revision catch-up without a ticker. Rust and Node verify the
same frozen canonical JSON/digest fixture; a deliberate encoder key-order mutation proves the digest
assertions fail. Existing REST fixture bytes remain unchanged.

Golden responses cover resolve matrix, renewal eligibility, descriptor dates and forever-readable prices; promotion versions are deferred with promotions (D-409) and are not part of this DoD until they return. Rating and Subscriptions consume these in separate plans; fixtures-crate deletion occurs in phase 4 (spec §10–§12).

Requirement: `cpt-cf-bss-pricing-fr-price-read`; PRD AC #19.

## 6. Acceptance Criteria

| DoD | PRD criterion | Given / When / Then |
| --- | --- | --- |
| `cpt-cf-bss-pricing-dod-resolve-matrix` | AC #18; `cpt-cf-bss-pricing-fr-resolve` | Given one item with EU/default prices, when resolve runs then both inputs are returned; an uncovered chain is explicit and no quantity total appears. |
| `cpt-cf-bss-pricing-dod-renewal-all-new` | AC #18; `cpt-cf-bss-pricing-fr-resolve` | Given pinned 10 → all 12 → new 15, when renewal resolves then it chooses 12 and signup 15; a forged foreign-chain pin is refused. |
| `cpt-cf-bss-pricing-dod-binding-sku-version` | AC #18; `cpt-cf-bss-pricing-fr-resolve` | Given an October 1 GL change already applied to the current SKU, when September resolves then it binds the earlier version; October binds the new one and prior pins do not change. |
| `cpt-cf-bss-pricing-dod-price-read-forever` | AC #19; `cpt-cf-bss-pricing-fr-price-read` | Given a closed price id from an old invoice, when read then its original money is returned; unknown/foreign ids reveal no price. |
| `cpt-cf-bss-pricing-dod-period-slices` | AC #20; `cpt-cf-bss-pricing-fr-quote` | Not built (D-415). Given a temporary price ending October 11 inside October 5–November 5, when preview runs then two slices appear; their common-price floors are not charged twice. |
| `cpt-cf-bss-pricing-dod-quote-totals` | AC #20; `cpt-cf-bss-pricing-fr-quote` | Not built (D-415). Given valid quantities and a promotion, when quote runs then totals apply the prorated floor and the promotion afterward (no included quantity since D-467); invalid quantities fail without changing pins. |
| `cpt-cf-bss-pricing-dod-events-typed-outbox` | AC #14; `cpt-cf-bss-pricing-fr-events` | Given approve/reject/withdraw/quorum-zero outcomes, when committed then each has its terminal event and only successful apply has its domain publication; rollback has neither. |
| `cpt-cf-bss-pricing-dod-consumer-golden-contracts` | AC #19; `cpt-cf-bss-pricing-fr-price-read` | Given stored contract fixtures including negative tenant/uncovered cases, when either backend serves the public paths then responses match; a shape drift fails the contract gate. |

Verification uses domain tests, scoped repository tests on both backends and REST positive/denial/precondition probes as applicable. Phase 2 checks must not mark later-phase behavior implemented. Golden consumer contracts belong to phase 4.

### Durable accepted terms (D-507)

The SDK acceptance command now uses the real read projection and pure compatibility validators,
with detached live Products/meter observations and bounded local-generation recapture. Acceptance,
authenticated command receipt and local audit commit atomically. Authorized retries keep the exact
original terms and deadline; another key on the same order line/version shares that receipt, while
changed intent conflicts. The acceptance fixture obtains its query from a real SDK resolve and
covers concurrency, provider failure, transaction rollback/restart, price/revision races and frozen
history. Receipt schema 1 remains stable. D-508 supplies hold and fresh fulfilment eligibility;
acceptance alone never grants activation, and the public release awaits the complete G3 gate.

### Frozen fulfilment (D-508)

The production SDK providers retain the accepted entry, policy, price, SKU descriptors and invoice
inputs through successor publication, deprecation and off-sale. Every fresh eligibility check
compares the exact receipt axes, digest and current market, checks live retirement and original
price closing metadata, and applies server-time expiry. Successor effective_to never ends the
accepted binding; explicit and temporary ends do, at their UTC boundary. A delayed first activation
inside [start_at, hold_until) is allowed and pinned by the hold. Hold and command insert atomically;
exact replay remains available after expiry, and another key cannot renew the deadline.

The fulfilment_holds suite exercises these rules, provider failure, generation/clock races,
concurrent holds, rollback/restart, and successor revisions selecting another policy window.
Subscriptions keeps its own committed order and attempt fencing. Eligibility observations are
never reusable admission tokens, and Pricing does not implement downstream activation.

### Executable Pricing consumer fixtures (D-509)

The schema-1 fixtures vm-hour, cloudlets-hourly-volume, cloudlets-hourly-graduated, frozen-acceptance
and unsupported-terms execute in `pricing_seam_contract`. Typed round trips and complete pin
comparisons cover real entry creation, price approval, plan publication, all seven ClientHub methods,
REST reads, authorization, mixed windows, shared-entry policy reuse and exact-policy book remapping.
Mutation probes must fail when expected money changes or a fixture policy pin escapes comparison.

F02/F23/F24 money checks are catalog representation checks using domain::money, with no Rating
scheduler, invoice calculation, VM provisioning or hourly event claim. Those atlas consumer obligations
remain specified/unexecuted here. The one provider behind `UsageMeterSemanticsV1` is Products'
(products P-D-233): it answers derived meters from Products' derived usage type at its exact version
(E1b), and raw meters stay unconfigured until Types Registry declarations answer through the Usage
Collector (E1a); E1a remains a test declaration in these tests until that provider is delivered.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).
Commands stay SDK-only; later remote transport binds these exact ports. Task 8
and the shared G4 controller gate complete the implementation handoff after this provider fixture slice.
