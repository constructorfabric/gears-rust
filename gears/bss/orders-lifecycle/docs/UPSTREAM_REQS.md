<!-- CONFLUENCE_TITLE: [BSS]: Orders Lifecycle — Upstream Requirements -->
<!-- Related: ./DESIGN.md, ./DECISIONS.md, ./features/ | Owners: BSS Orders team -->

# UPSTREAM_REQS — Orders Lifecycle

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Requesting Gears](#12-requesting-gears)
- [2. Requirements](#2-requirements)
  - [2.1 Subscriptions](#21-subscriptions)
  - [2.2 Rating / price evaluation](#22-rating--price-evaluation)
  - [2.3 Billing chain](#23-billing-chain)
  - [2.4 Account Management](#24-account-management)
  - [2.5 Payments](#25-payments)
  - [2.6 Orders Workflow](#26-orders-workflow)
  - [2.7 Event Broker](#27-event-broker)
  - [2.8 Identity platform](#28-identity-platform)
  - [2.9 Platform authorization policy](#29-platform-authorization-policy)
  - [2.10 Catalog registry (Product & SKU)](#210-catalog-registry-product--sku)
  - [2.11 Contracts](#211-contracts)
- [3. Priorities](#3-priorities)
- [4. Traceability](#4-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

What this gear needs from gears it does not own, declared here so a future specification of those
gears is authored with these obligations visible. Until now these asks lived only in slice prose,
which is how the `SUB-O*` numbering forked between the Subscriptions seam map and the sibling
Orders Workflow PRD ([`DECISIONS.md`](./DECISIONS.md) Q-04).

### 1.2 Requesting Gears

| Requesting gear | Why it needs the target |
|-----------------|-------------------------|
| `orders-lifecycle` | Owns the order document and its state machine; needs Subscriptions to accept an explicit start instant, expose an overlap-occupancy read, and carry an order reference and a compensation cancellation reason. Needs Pricing to read the seller's catalog frontier and catalog data with the catalog tenant named explicitly. Needs Rating to expose a batched fixed-version evaluation SDK, a pre-subscription evaluation and an annualised TCV figure. Needs the billing chain to propagate the external reference and to answer an indicative tax read. Needs Account Management to issue verifiable delegation proof and expose the payer's commercial profile, Contracts to answer contract status, party eligibility and the acceptance-required declaration for a referenced contract, and the platform PDP to evaluate it from request context with distinct missing/invalid deny reasons. Needs the catalog registry to expose each plan/SKU's `catalogSubscriptionProductKey` at a given catalog version. Needs a Payments capability that does not exist, and needs the Event Broker runtime behind the
landed SDK before event-producing traffic can be accepted. |
| `orders-workflow` | Must consume `OrderAmended`, obtain the approval-requirement verdict for the new order version, and reflect the new version onward from `submitted`; without this the Lifecycle two-step re-approval seam stalls. |

## 2. Requirements

### 2.1 Subscriptions

The seam-map numbering (`SUB-O1`…`SUB-O6`) is treated as canonical here, per Q-04. Of the
identifiers the Workflow PRD adds beyond the seam map, **only `SUB-O9`** (correlation
propagation) is an ask of this gear; it is carried below and flagged as unregistered upstream,
since the seam map does not define it. **`SUB-O4`, `SUB-O6`, `SUB-O7` and `SUB-O8`
are not asks of this gear** and are deliberately absent from this register — `SUB-O6` notably so,
since Q-04 records it carrying two different meanings across the two registers. `SUB-O3` is
registered below but is a **preservation** ask rather than a change request, which is why the
slices that rest on it do not count it among their unagreed dependencies.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-subscription-start-instant`

The activation intent and `create` **MUST** accept an explicit **start instant** and **MUST NOT**
derive the subscription start from any date carried on the order. Where the two-phase activation
barrier defers a line past its quoted service-activation date, the spawned subscription's start is
the **actual activation instant**; billing and entitlement **MUST NOT** be backdated to the
earlier quoted date. Raised as **`SUB-O10`** — a new ask, not present in either existing register.
Without it the PRD's no-backdating requirement is unenforceable from the order side, because
Subscriptions owns the start. See [`DECISIONS.md`](./DECISIONS.md) D-56.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-overlap-presence-read`

An **overlap-key occupancy read**: batched, and for each `(payer_tenant_id, overlap_scope_key)`
returning `(activeCount, maxConcurrentActive, provenance)` — the number of subscriptions `active`
on that key (drafts excluded), the effective concurrent-active cardinality, and the Catalog/Contract
policy that cardinality was resolved from. Registered upstream as **`SUB-O5`**, unagreed.
**This is an amendment to `SUB-O5`**, whose upstream text asks for presence ("this gear answers
presence"): a boolean cannot evaluate `activeCount + proposed ≤ maxConcurrentActive` when the limit
exceeds one, so this design asks for the count and the limit; the Subscriptions gear's own seam map
is not edited here ([`DECISIONS.md`](./DECISIONS.md) D-126). The requirement ID is kept for
stability. The against-existing-subscriptions half of the
submit gate's overlap predicate depends on it; until it lands that half is unevaluable and
therefore a refusal, which fails closed.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-compensation-cancel-reason`

A cancellation **reason value for order-fulfillment compensation**, scoped **out** of the
early-termination class so it derives neither a termination fee nor a credit. Registered upstream
as **`SUB-O1`**, marked critical there, unagreed. The upstream note records that reason values
ride event payloads consumers key on, so adding one after Billing consumes the contract is a
breaking change — making this the ask materially cheaper now than later.

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-order-reference-on-create`

An optional **order reference** (`orderId` plus order-line reference) accepted on `create`, so
"which order produced this subscription" is answerable from the subscription side. Registered
upstream as **`SUB-O2`**, unagreed. This gear already persists the forward mapping; without the
reverse, provenance is one-directional and a subscription created outside the order path is
indistinguishable from one created through it.

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-two-phase-pair-preserved`

The `draft → activate` pair **MUST** remain externally callable, with the `draft → cancelled` void
remaining not resource-affecting. Registered upstream as **`SUB-O3`**. No change is requested —
only that the contract already established is not collapsed into a create-and-activate
convenience, because order-level atomicity is built entirely on it.

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-correlation-propagation`

The process **correlation identifier** echoed on confirmations and propagated toward the Policy
Engine and OSS. Cited by the sibling Workflow PRD as `SUB-O9`; **not present in the seam map**, so
unregistered upstream. Without it an end-to-end acquisition trace stops at the seam.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-overlap-activation-atomicity`

**Atomic enforcement of `overlapScopeKey` at the point a subscription commits to `active`.** The
order axis of the overlap rule is closed inside this gear's transition transaction by
[01 §3.7](DESIGN.md#contract-01-3-7)'s partial unique index, but the **subscription** axis cannot be: the committing
transaction belongs to Subscriptions, so no re-check performed here can be atomic with it. Two
activation waves can therefore each pass this gear's re-check and jointly exceed
`maxConcurrentActive`. Subscriptions **MUST** re-evaluate the key and commit `active` under one
reservation or serialisation boundary. Until it does, **the gap is open and [03 §2.2](DESIGN.md#contract-03-2-2) does not
bound it** — that section states why no timed validity window is assertable from this side, since
nothing this design declares carries a deadline to the party that would have to honour it, and
`spawn-signal` is event-less. What holds meanwhile is narrower: the re-check is an **early abort**
with no admission guarantee. A collision found before `active` is a per-line rejection and a
pre-activation abort; one appearing at or after `active` is a fulfillment failure carrying
`overlap-collision` on the failure-acknowledgement path, whose compensation evidence must show no
active subscription remains ([03 §2.2](DESIGN.md#contract-03-2-2), `DECISIONS.md` D-89). This is the same seam as `SUB-O5`, which supplies the *read*; this ask is
the *enforcement*, and the read alone does not make the rule hold.

### 2.2 Rating / price evaluation

**Pricing SDK readiness (OL-51/52).** The existing
`PricingCatalogClientV1::pin_frontier` is reused. Pricing already implements tri-state
sellability outcomes internally/through REST, including active-window coverage/horizon.
GA/prepaid and registry-sellability inputs remain unevaluable. Bundle evaluation has an additional
integration gap described below; the ordinary predicate roster is not proof of bundle coverage.
Pricing owners must expose batched predicates against an explicit committed catalog version
and pin composition through the public SDK, preserving predicate identity, `Failed` versus
`NotEvaluable`, diagnostic detail and missing-input ownership. Orders normalizes these to its
registered reasons; it must not import Pricing internals, recreate predicates, or substitute
per-line latest-frontier calls. The required input/output contract and failure mapping are in
[03-gate-and-pin](DESIGN.md#contract-03-1-1) §§1.3, 4.1. Verify whole-basket (up to 200 lines) behavior, unavailable
and absent frontier, noncommitted versions, unresolved composition, and deadline exhaustion.
These SDK operations and shared circuit-breaker support remain open integration prerequisites;
the 2.25 s submit / 2.5 s Preview resolution ceilings are baselines, not measured latency.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pricing-catalog-tenant-reads`

**Catalog reads naming the catalog tenant.** `PricingCatalogClientV1::pin_frontier` reads "the
caller's tenant pin-eligibility frontier" and returns `PermissionDenied` when the PEP denies
(`pricing-sdk/src/api.rs`). The order gate must read the **seller's** catalog, and on the partner
and direct paths the caller's tenant is not the seller. Pricing owners must expose
`pin_frontier_for(ctx, catalog_tenant_id)` — the pin-eligibility frontier of the named catalog-owner
tenant, `None` when that tenant has no pin-eligible version — and the same explicit
`catalog_tenant_id` parameter on the other catalog reads the gate performs: the batched
fixed-version predicates, pin composition and the reference resolution behind them. The
reference-resolution/predicate answer must return, per line, the **market scope (currency and
region) of the resolved price row** at the fixed version, which is the source of a line's region
for the order-market predicate (D-124). A PEP denial must stay distinguishable from outage so
Orders can record an operator diagnostic; Orders maps it to `catalog-frontier-unavailable` (or the
port's own unavailable reason) and never surfaces it to the buyer as 403. Until exposed, Orders
uses `pin_frontier` only when the caller's tenant is the seller and otherwise refuses
`catalog-frontier-unavailable` ([03 §2.2](DESIGN.md#contract-03-2-2); [`DECISIONS.md`](./DECISIONS.md)
D-122).

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-rating-evaluation`

**A consumable Rating evaluation SDK.** No Rating SDK crate exists, so the price-evaluation
contract the gate calls in [03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit* step 5 has no
client. Rating owners must expose, through a public SDK resolvable from `ClientHub`, an ordinary
batched evaluation of a whole basket (up to 200 lines) at an explicit committed catalog version of
a named catalog tenant, returning the non-authoritative resolved total per line and per order — gross, net, discount
component and promotion reference, the four charge kinds — and the TCV figure whose semantics
`…-upreq-tcv-with-annualisation` defines. Subscription-scoped overlays remain the separate
`…-upreq-pre-subscription-evaluation` ask. Until exposed, every submit, amendment and Preview
refuses with `evaluation-unavailable` (fail closed, ADR-0003).

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pricing-bundle-sellability`

**Frozen bundle composition and component conjunction.** Pricing owners must include bundle
classification, price basis and the frozen component/key roster in the committed snapshot,
advance the published version when composition changes, and wire component-conjunction
execution into the fixed-version predicate SDK. The current
[`bundle_sellability.rs`](../../pricing/pricing/src/domain/bundle_sellability.rs) implements the
rule but documents no caller; [`sellability.rs`](../../pricing/pricing/src/domain/sellability.rs)
documents missing frozen composition, `sum_of_parts` rejection without component evaluation
and `own_price` evaluation that omits its components. SDK publication alone is insufficient.
This is an open Pricing implementation prerequisite, not a new Orders evaluator.

The typed response must identify the offered plan and every evaluated component/key, with
canonical key encoding, complete per-subject outcomes and missing-input ownership. Apply the
owning bundle rule: component predicates (1)–(5), bundle-level registry sellability, and the
bundle's own rows where its price basis requires them. Component registry sellability is not
a purchase prerequisite; composition-only SKUs must remain usable as bundle components.
Preserve distinct results under Gate §3.7's evaluation identity and ordering.

Orders must fail closed for bundle assessments without verified complete pinned coverage.
An explicit upstream missing-composition/unevaluated-conjunction answer is `not_evaluable`,
normalized to `catalog-predicate-unevaluable` with Pricing's diagnostic ownership. Missing
required roster/coverage metadata or omitted required results makes the SDK response unusable
and maps to `catalog-predicates-unavailable`; do not accept a passing bundle-own-row result
as a substitute for its components. A known empty composition is an evaluated failure, distinct
from an unavailable composition. Orders performs no component walk or commercial predicate
reimplementation locally. No fallback to current mutable composition is permitted.

Acceptance must cover both price bases; a failing or unavailable component; composition-only
SKUs; multiple components/keys sharing a predicate; missing versus known-empty rosters;
composition changes after pinning; and exact diagnostic persistence/replay. Until the owning
snapshot and SDK coverage contract land, bundle submit/amendment is refused and Preview reports
the unevaluable/unavailable outcome. This does not turn ordinary plan evaluation into bundle
conformance or claim that any production path is ready.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pre-subscription-evaluation`

A **pre-subscription resolved-total evaluation** operation: given the order's scope inputs — plan,
price and SKU references, quantity, currency, region, contract reference and term — produce the
resolved total for price-list scopes that need subscription-level context. PRD §13 names this
**required** for the `p1` price-evaluation dependency, and PRD §15 row 6 is the open question it
closes. Until it exists, overlays requiring subscription-level context are **excluded** from the
order-time total and the exclusion is stated on the read and Preview responses
([03 §4.5](DESIGN.md#contract-03-4-5)) — a truthful interim, but one
that becomes permanent by default while no ask is registered.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-tcv-with-annualisation`

The **net pre-tax total-contract-value figure**, computed by the evaluation domain and carrying the
PRD's semantics: usage excluded, one-time charges counted once, and an open-ended term annualised
at the line's billing cycle (12 monthly, 4 quarterly, 1 annual — PRD §12 AC-2d). D-40 moved this
computation off this gear to satisfy R4's prohibition on price arithmetic, which places the
obligation here; without the ask, TCV for every rolling deal is undefined and two rolling deals
differing only in billing cycle become incomparable at the approval gate — the exact comparison
the annualisation exists to enable.

### 2.3 Billing chain

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-external-reference-propagation`

The **external reference** carried onto billing documents. PRD §13 makes it a `MUST` — "External
reference on the order/line **MUST** propagate to billing documents" — for buyer-side
accounts-payable reconciliation. This gear publishes it on `OrderSubmitted` and `OrderCompleted`,
but billing documents derive from the **subscription**, not from order events, so the hop past the
event payload is unspecified. Either the activation intent must accept it and Subscriptions carry
it onto the billable facts, or Billing must consume it from the order events directly and say so.
A purchase-order number that never reaches the invoice fails the requirement invisibly: the order
shows it, the event carries it, and only the invoice lacks it.

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-indicative-tax-read`

An **indicative tax read** — per line and in total for a basket — from the billing-chain tax
owner, which PRD §9.1 requires Preview to return and this design never stores. No gear or
specification for a tax owner exists in this repository, so this ask has no target to register
against and is recorded here for whichever specification takes it.

### 2.4 Account Management

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-delegation-proof-credential`

A **verifiable delegation-proof credential**: a signed assertion naming the delegating tenant, the
delegated scope, the delegate, an issue instant and a finite expiry; verifiable against a published
issuer key; revocable by the delegating tenant with revocation observable at verification time.
Aligned with BSS manifest §2.1.3. This is the single control preventing cross-tenant leakage on the
partner-placed path, and it has no specified form today. The platform PDP, not Orders, verifies it
(`…-upreq-pdp-policy-integration`). See [`DECISIONS.md`](./DECISIONS.md) D-32, D-111.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-payer-commercial-profile`

A read of the **payer's commercial profile** yielding the `(currency, region)` binding the order
market is derived from. Consumed at submit and re-read before the first activation intent. It is
`p1` because predicate 4 (order-market consistency) sits on the `p1` submit path. The existing
`AccountManagementClient::get_tenant` serves tenant-axis validity; no Account Management operation
returns a commercial profile, so until this is exposed the identity outcome is
`identity-party-unavailable`. Party eligibility is **not** asked of Account Management: it is
owned by Contracts (§2.11).

The profile **must also state the payer's commercial relationship with a named seller tenant**:
whether the payer is in a commercial relationship with the order's `sellerTenantId`. An amendment
that changes `payerTenantId` reads this answer to decide whether the change crosses seller scope
([04 §2.2](DESIGN.md#contract-04-2-2)), and refuses `payer-rebinding-requires-seller` unless the
relationship is confirmed. No separate tenant-hierarchy operation is requested. See
[`DECISIONS.md`](./DECISIONS.md) D-128.

### 2.5 Payments

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-authorization-outcome`

An **authorization outcome** for a payer, distinguishing **authorized**, **pending** and **failed**
as three separate answers — a three-valued outcome; only `authorized` and `failed` are expected on
begin-fulfillment, and Lifecycle refuses a `pending` submission `authorization-pending` as a
defensive branch, not a protocol step (D-131). Consumed as a begin-fulfillment guard input and never stored as an order
fact. **No Payments capability and no specification exists in this repository**, so this ask has no
target gear to register against; it is recorded here so that a future Payments specification is
authored with it visible. Capture, settlement, strong-customer-authentication and refunds are
explicitly **not** requested — see [`DECISIONS.md`](./DECISIONS.md) Q-08 for the consequence.

The Payments contract must also provide an idempotent request identity and read-by-request
outcome so Workflow can resume a `pending` authorization without submitting a new charge or
losing the original attempt. Workflow owns bounded durable polling/escalation through its
selected execution platform ([05 §4.3](features/05-preconditions.md#contract-05-4-3)); Orders does not add a Payments
timer. Configuration, restart recovery and exhaustion tests are prerequisites, not delivered
capabilities. Conclusive `failed` outcomes are sent to Lifecycle, the sole evaluator of its
tolerate-failure policy; Workflow must not independently suppress that transition request.

### 2.6 Orders Workflow

**Recheck and progress integration (OL-29/30/59).** Reuse Workflow PRD §6.1's owning-SDK
overlap check at plan construction and market/overlap check immediately before activation,
after Lifecycle has committed `in_fulfillment`. No new Lifecycle check endpoint is required.
The public Subscriptions/identity SDK contracts must supply those inputs; false predicates
void wave-1 drafts and lead to failure acknowledgement, while unavailable inputs return `defer`:
Workflow retries under `activation-recheck-retry-budget` (baseline 3 attempts over ≤ 60 s) and, once
it is exhausted, acknowledges failure with the port's unevaluable reason; a held, terminal or
superseded order returns `not-dispatchable` and is re-read, never acknowledged ([03 §3.6](features/03-gate-and-pin.md#contract-03-3-6), D-127).
Lifecycle's own re-check inputs — each line's stored `overlap_scope_key` and the version's
`market_currency`, `market_region` and `payer_tenant_id` — are carried by Lifecycle's composed
order read as fulfillment inputs ([08 §4.2](DESIGN.md#contract-08-4-2), D-144); Workflow needs no
further Lifecycle read.
Subscriptions' batched overlap read must expose active occupancy and effective
`maxConcurrentActive` with Catalog/Contract policy provenance; missing limits fail closed,
never default to one. Activation-time atomic enforcement remains owned by Subscriptions.

Workflow's existing PRD §9.1 progress-read operation is the UI's source of intermediate task
progress; Lifecycle's line projection records acknowledgements only. Verify its public SDK,
PDP scope and order/version correlation before enabling the combined UI. This is a specified
Workflow capability, not proof of runtime implementation. Any Lifecycle PRD interpretation
requiring live progress from Lifecycle itself must be reconciled with this ownership split.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-workflow-amendment-verdict`

On consuming `OrderAmended`, Orders Workflow **MUST** terminate or supersede prior-version
processing, obtain the approval-requirement verdict keyed by the event's `orderId` and new
`orderVersion`, and reflect that verdict into Orders Lifecycle: `submitted → pending_approval` for
approval required, or `submitted → approved` where it is not required. It **MUST NOT** carry the
prior version's verdict forward. This is required by the Lifecycle two-step amendment seam; until
the reflection arrives, the order remains `submitted` and its submitted TTL continues to run.

The current Workflow PRD restricts verdict acquisition to `OrderSubmitted`; it therefore needs an
amendment before this seam can be implemented. See `DECISIONS.md` Q-12.

**Workflow reconciliation (OL-26/37/57/58/61).** On amendment, acceptance and authorization are
evaluated for the new version; historical assent cannot satisfy it. Durable pending-authorization
continuation, hold/resume and superseded-version cancellation belong to Workflow's selected
execution platform, which remains Q-10, not an Orders-owned scheduler. Completion evidence must
cover the exact persisted current-version line roster, with no omissions, extras or duplicates.
Workflow must obtain a committed `report-spawn-signal` before dispatching activation and recover
that result after ambiguous replies. This closes **order** direct cancellation before dispatch;
it is not the Workflow task's potentially later unilateral-cancellation boundary. The Workflow
PRD wording must be reconciled by its owner before release; this document does not claim that
Product has approved the divergence. See [06 §4.3](features/06-workflow-seam.md#contract-06-4-3).

### 2.7 Event Broker

**Consumer freshness integration (D-67 / Q-25).** Workflow, Subscriptions and Billing owners
must implement Foundation §4.4's applicability read before business effects, with explicit
PDP `order × read` grants constrained to their authorized target orders. Provisioning and
verifying those grants is owned by §2.9 (`cpt-cf-bss-orders-lifecycle-upreq-pdp-policy-integration`);
this section covers consumer-side read and retry behaviour. Platform-root event
access supplies neither these grants nor business authorization. Use the existing Orders read
surface; no new endpoint is requested. Unavailable/denied validation retains durable pending
work, retries under bounded consumer policy and escalates without effects or silent loss.
Each consumer must declare applicability per event/intended action: a hold can defer work,
and a historical financial effect need not be obsolete merely because state advanced. A
successful read proving that action obsolete may retire it; execution still uses downstream guards.
Product/Architecture must reconcile PRD §9.2's no-callback wording under Q-25 and account for
read load and availability in integration acceptance. This remains open consumer/deployment
work, not evidence of implemented grants or retries. Verify delayed/duplicate events, recovered
dead letters, unavailable reads, missing grants and restart during validation.

**Shared documentation follow-up — open (2026-09-22).** Event Broker SDK and GTS guideline
maintainers should reconcile `guidelines/GTS.md` with the canonical declarations in
[`event-broker-sdk/src/gts.rs`](../../../system/event-broker/event-broker-sdk/src/gts.rs): event
base `gts.cf.core.events.event.v1~`, business content in `data`, the closed `EventTraits`
vocabulary, topic-level retention, and publish-required envelope members. The old
`gts.cf.core.events.type.v1~` spelling also appears in `gears/settings-service/docs/DESIGN.md`;
its owners must review that contract separately. Closure requires correcting the guideline
examples and reviewing affected gear references against the SDK. Orders' corrected contract
and implementation acceptance criteria are in Foundation §4.7; this follow-up requests no new
SDK capability, changes no other gear, and does not assert that a platform ticket was filed.

The same follow-up includes the guideline's custom-error examples: `with_type_uri` is not a
supplied builder, and platform `Problem` uses canonical category URIs rather than a gear's
reason identifier as `type`. GTS guideline, canonical-errors and contract-macros maintainers
should align examples with `#[derive(ContractError)]`, canonical status/title selection and
`error_domain`/`error_code`, distinguishing derived error types from instances. Foundation
§4.7 now follows the supplied implementation; the shared guideline correction remains open.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-event-broker-runtime`

A production `EventBrokerApi` runtime **MUST** be available through `ClientHub` and support the
managed chained producer protocol used by `event-broker-sdk::DbProducer`. Deployment **MUST**
publish the actual topic partition count so Orders can configure and verify it before readiness.
The integration gate **MUST** cover eager GTS schema preparation, producer registration/cursor
recovery, accepted/duplicate outcomes, transient retry and permanent rejection.

This is an explicit release blocker, not a request for Orders to implement the broker. The current
platform inventory says “SDK landed (`cf-gears-event-broker-sdk`) — impl crate TODO”
([`../../../../docs/GEARS.md`](../../../../docs/GEARS.md)). Orders can test against an
`EventBrokerApi` double, but it **MUST NOT** report ready for production event traffic without the
runtime.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-event-broker-cursor-retry`

**Transient producer cursor-recovery failures.** The Event Broker SDK's managed Chained
producer **MUST** classify transport and rate-limit failures during initial cursor recovery as
`MessageResult::Retry`. These failures **MUST NOT** dead-letter the queued event or advance the
toolkit queue-partition cursor. Permanent failures retain the SDK's rejection policy; retry
cadence remains owned by toolkit-db.

**Owner:** Event Broker SDK maintainers.

**Current gap:** `ProducerOutboxProcessor::handle` converts every error from
`event_for_message` into `Reject`, including transient failures fetching the initial producer
cursor. See the SDK's
[`producer/outbox.rs`](../../../system/event-broker/event-broker-sdk/src/producer/outbox.rs).
An empty cursor cache after restart or worker takeover can therefore turn a temporary broker
outage into permanent rejection of valid events.

**Release gate:** Orders production deployment **MUST** use an SDK revision containing this
fix. Closure **MUST** record the merged SDK fix PR, the deployed revision containing it and
passing regression evidence. This is a release prerequisite, not a runtime health check.
**Tracking status:** open; no SDK fix PR or qualifying revision has been recorded.

**Acceptance evidence:** regression coverage **MUST** start with an empty cursor cache and
inject transport and rate-limit cursor-fetch failures. Each failure **MUST** retain the message
for retry without dead-lettering or advancing the queue-partition cursor. After broker recovery,
the original event **MUST** publish with its event ID unchanged. A permanent cursor-recovery
failure **MUST** still follow the rejection policy.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-event-broker-dead-letter-recovery`

**Operator recovery of dead-lettered producer events.** The platform **MUST** provide an
authenticated operator interface to inspect and recover Event Broker SDK producer messages from
toolkit dead letters after the failure's cause has been corrected.

**SDK responsibility:** safely republish the original event, preserving its event ID and business
payload while handling producer identity and chained sequencing, including when the original
producer sequence can no longer be reused. Recovery **MUST NOT** require another Orders business
transition or alter order state. The SDK **MUST** mark the dead letter resolved only after broker
acknowledgement. Recovery **MUST** remain safe if publication succeeds but recording resolution
fails, including repeated recovery requests and concurrent claims. Unsuccessful recovery **MUST**
leave the message recoverable and visible to operational monitoring.

**Operations tooling responsibility:** provide a shared platform CLI, UI or job with authorization
for inspection and recovery, controlled payload access, an operator-supplied recovery reason and
an audit trail recording the actor, message identity, action and outcome. It **MUST** report broker
acceptance separately from downstream consumer processing; resolving a dead letter proves only
the former. Orders supplies queue configuration, alerts and its recovery runbook.

**Owners:** Event Broker SDK maintainers and platform operations tooling maintainers.

**Current gap:** toolkit's `dead_letter_replay` claims records for reprocessing; it does not
republish them. See [`outbox/mod.rs`](../../../../libs/toolkit-db/src/outbox/mod.rs).
The supported SDK republication mechanism and shared operator interface remain open dependencies.
Removing the Orders re-drive endpoint does not itself supply either capability.

**Release gate:** Orders production deployment **MUST** have both the supported SDK recovery
mechanism and an operator interface. Closure **MUST** record their implementation PRs/deployed
revisions, the operator runbook and passing acceptance evidence.
**Tracking status:** open; no qualifying implementation references have been recorded.

**Acceptance evidence:** recover a rejected `OrderCompleted` after correcting its cause without
changing the completed order. Verify stable event ID and business payload, recovery after the
producer chain has advanced, safe retry after publication succeeds but resolution is interrupted,
and safe concurrent recovery requests. Verify unauthorized inspection/recovery is denied, actions
are audited, and failed recovery remains visible and recoverable.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-event-broker-root-tenancy`

**Explicit platform-root tenancy for internal Orders events (D-95).** The platform **MUST**
provide the canonical platform-root tenant UUID through an authoritative, documented source
available before Orders accepts event-producing traffic. The SDK already supports explicit
envelope tenancy through `TypedEvent::tenant_id()`; this requirement does not request a new
override API. `ROOT_TENANT_ID` is the conceptual identity, not an assumed exported SDK constant.

Event Broker **MUST** honor the explicit envelope tenant when the producer is authorized for it,
independently of its service-context tenant, and enforce the service grants in
[authorization contract](DESIGN.md#contract-08-4-3).
The platform **MUST** reconcile the broker design's producer-supplied tenant contract with wording
that describes recording the publisher-context tenant. Root-scoped events **MUST NOT** become
readable merely because a principal has customer, partner or seller access to an Orders API.

**Owners:** Event Broker, platform tenant identity and authorization maintainers, with the
Workflow, Subscriptions and Billing owners confirming their consumer grants.

**Release gate and tracking:** open. Orders production deployment requires a documented root UUID
source, the concrete producer/consumer identities and grants, and passing broker integration
evidence. No verified identity source or deployed grant configuration has been recorded.

**Acceptance evidence:** publish from an authorized service context with an explicitly selected
root tenant and verify that envelope tenancy survives enqueue, publication, recovery and consumer
delivery. Verify Orders event partitioning remains keyed by `orderId`, not the root UUID. Verify
authorized service consumers can read their permitted events, unauthorized production and
consumption are denied (including root-tenant principals without grants), and customer/partner/
seller API roles cannot read the internal stream. Downstream action tests **MUST** demonstrate
that root event access does not bypass business tenant authorization.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-event-delivery-observability`

**Delivery monitoring for the Orders producer queue.** The platform **MUST** expose supported
measurements that identify delayed delivery and pending dead letters for `bss-orders-events`,
including pending queue depth, oldest pending message age, retry counts and pending dead-letter
counts. Event Broker publication outcomes and commit-to-broker-acceptance latency **MUST** be
observable, including queue wait and retries. The timestamp source, clock alignment and any
approximation error **MUST** be documented in accordance with
[`DESIGN.md §4.1`](./DESIGN.md#41-capacity-and-cost); enqueue time **MUST NOT** silently stand in
for commit time. Orders operation timing supplies the remaining request-to-commit portion of the
full-path measurement.

**Owners:** toolkit-db maintainers provide generic queue measurements; Event Broker SDK
maintainers provide publication outcomes and timing. Orders owns its delivery objective,
queue-specific dashboard/alerts, thresholds, evaluation windows and recovery runbook. Platform
operations connects alerts to the responsible operator and shared recovery tooling. Orders uses
supported platform measurements rather than a separate monitoring subsystem or private outbox SQL.

**Acceptance evidence:** a simulated broker outage **MUST** make backlog and increasing oldest
pending age visible and trigger the configured delayed-delivery alert. Transient failures **MUST**
appear in retry measurements; a permanent rejection **MUST** remain visible as a pending dead
letter and trigger its alert. Restoring publication **MUST** drain pending work; a dead-letter
alert **MUST NOT** clear merely because later events succeed. Verify its resolution through the
platform recovery procedure. Measured latency **MUST** include waiting and retry time, including
across worker restart, rather than only the successful publish call. Pending and dead-lettered
events **MUST** remain visible alongside completed-delivery percentiles.

**Release gate and tracking:** open. Before production, record the supporting platform
implementation revisions, measurement method, Orders alert configuration and runbook, and passing
acceptance evidence. Current worker execution statistics alone do not meet this requirement.
Numeric latency acceptance remains governed by the PRD and Q-16; this requirement neither adopts
the unapproved 30-second proposal nor chooses a replacement target. Delayed-delivery and
dead-letter detection remain mandatory independently of Q-16's outcome.

### 2.8 Identity platform

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-audit-identity-lifecycle`

**Shared identity follow-up (D-103; supersedes D-102's Orders-only p1 gate).** Orders uses
`SecurityContext.subject_id()` as its immutable actor reference, as Pricing does. The current
contract relies on platform principal stability and identity lifecycle; Orders does not invent
an issuer namespace, identity mapping service or profile lookup. This follow-up is open, not
satisfied by declaring the architecture, and is shared with Pricing rather than a prerequisite
for choosing Orders' actor representation. Existing deployment security/privacy obligations and
a confirmed unsafe identity configuration still require resolution; this reclassification is not
a waiver or a statement that the platform guarantees have been verified.

**Verified surfaces (2026-09-21, upstream `8aca4d6df17c90f8ecb8b0195e4db4ec40921937`):**

| Surface | Evidence and boundary |
|---------|-----------------------|
| Actor source | `libs/toolkit-security/src/context.rs` exposes subject UUID, home-tenant UUID and optional subject type; Pricing's `api/rest/auth_context.rs::audit_stamp` records the subject UUID |
| Namespace | SecurityContext has no issuer field; a UUID type does not establish cross-issuer uniqueness or non-reassignment |
| Identity ownership | Account Management ADR-0005 assigns identity data to the deployment IdP; AM coordinates lifecycle without a local user table |
| Deletion | Keycloak IdP plugin `domain/user_facade.rs` implements tenant-checked deletion and configurable session revocation, not evidence of erasure across every backup/cache/restore |
| Jobs | Pricing's window-activation job does not write its nil actor to the audit log and acknowledges a tamper-evidence gap; Orders does not copy that gap |

**Shared platform questions**: AuthN/IdP owners should document stable identity across issuers,
reprovisioning and migration, non-reuse, and trusted namespace handling if needed. AM/deployed IdP
owners should document deletion versus disablement, session behavior, resolution permissions and
backup/cache/restore lifecycle. Privacy/Legal owns the applicable retention/removal policy and
residual linkability assessment; pseudonymisation is not automatically anonymisation. These
answers should apply consistently to Pricing and Orders, not produce separate gear-local identity
systems. Accountable deployment teams and supporting evidence remain to be confirmed; no external
request or approval is implied here.

**Orders-specific responsibility**: configure a trusted service identity for scheduler transitions,
derive the existing `system` actor class from that configured worker identity (the closed
`system`/`service`/`user` class of [01 §3.7](DESIGN.md#contract-01-3-7), D-115), and fail closed on
missing identity. Do not borrow the order creator's identity, accept caller-supplied actor values,
or store anonymous/nil IDs as real principals. Authenticated denials remain audited under D-98;
unauthenticated traffic belongs to the authentication boundary.

**Orders acceptance**: assert the actor equals the authenticated subject (canonical UUID text in
the existing text column), remains unchanged across credential rotation/retries, and contains no
names/emails. Verify reads and hashing without profile resolution; simulated profile removal must
leave retained audit bytes unchanged. Test configured system actors, missing configuration,
payload minimization and authorization. Provider non-reuse/deletion/restore evidence belongs to
the shared follow-up above, not to a new Orders-built mechanism. See
[`DESIGN.md §4.3`](./DESIGN.md#43-data-protection-residency-and-retention).

### 2.9 Platform authorization policy

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pdp-policy-integration`

**Ownership boundary: follow Pricing's integration pattern.** Orders declares its resource/action
catalog and trusted property inputs, requests decisions through `authz-resolver-sdk` PolicyEnforcer,
and enforces returned scopes and business guards. Platform authorization/deployment owners select
and operate the PDP provider, provision policies and role assignments, and supply the tenant and
delegation relationships those policies evaluate. Orders does not build a parallel evaluator or
infer permission from successful registration of an `AuthzPermissionV1` instance.

The authoritative Orders policy contract is [08 §3.5](DESIGN.md#contract-08-3-5) and §4.3: distinct
actions, resource/seller/current-payer access paths, complete alternative grants, payer-use authority,
and existing/proposed arrangement checks. The bounded trusted-maintenance exception there remains
separate; it does not exempt Workflow or public SDK/REST callers from PDP.

**Delegation-proof evaluation (D-111).** PDP policy, not Orders, evaluates delegation proof. The
concrete ask:

1. **Request carrier.** Orders passes the delegation proof reference the caller presented, unvalidated,
   as request context on every PolicyEnforcer call, reads and writes. Today `AccessRequest` carries
   resource properties and tenant context only, and `EvaluationRequestContext` has no
   caller-evidence field, so the platform must name the supported carrier (a request-context
   attribute, or bearer-token forwarding to the PDP).
2. **Policy evaluation.** Policy decides whether an authorized path needs delegation and whether the
   supplied proof is valid for it — issuer key, delegate, scope, expiry and revocation per
   `…-upreq-delegation-proof-credential`. Missing or invalid proof refuses only that path, never an
   independently complete non-delegated path.
3. **Distinguishable deny reasons.** A denial caused by proof carries a stable
   `DenyReason.error_code` that distinguishes *required proof absent* from *supplied proof invalid*
   (expired, revoked, wrong scope, bad signature), so Orders can map them to
   `delegation-proof-required` and `delegation-proof-invalid`. `EnforcerError::Denied` already
   surfaces `deny_reason`; only the codes need agreeing.
4. **Accepted proof reference (desired).** An allow response that names the proof reference the
   policy accepted, so the audit and access-log entry records verified rather than supplied
   evidence. Until then Orders records the supplied reference on any allowed request that carried
   one, and the entry means "supplied", not "verified" ([08 §4.4](DESIGN.md#contract-08-4-4)).
5. **Allowed-path marker (desired, D-140).** An allow response that says which authorization
   path it allowed — delegated or direct — so Orders can set `orders_order.sales_path` at create
   from the path itself. Until then Orders uses a proxy: `partner_placed` iff the allowed create
   request carried a delegation proof reference, otherwise `self_service`
   ([01 §3.7](DESIGN.md#contract-01-3-7)), counting only the proof the PDP accepted once item 4 is
   delivered. The proxy is imprecise in both directions (D-146): it over-classifies a caller who
   supplies a proof on its own-tenant create, and it cannot see delegation that begins after
   create, so Orders keys no acceptance control on `sales_path` alone — the submit-time automatic
   acceptance keys on the submit request's own facts and the recording-party bar also compares
   stored version actors' tenants ([05 §4.2](features/05-preconditions.md#contract-05-4-2)). It is replaced
   by the marker once this item is delivered. The marker is read for `sales_path` only; proof
   evaluation stays with policy (D-111).

Orders never classifies a path as delegated and never validates proof locally, so the delegated
arm of every Orders operation stays unbuildable until items 1–3 are provided.

**Acceptance evidence:** identify the deployed provider and policy/configuration revisions, then
jointly test principals in the same tenant with different action grants; each of the three read
axes; rejection outside all authorized paths; former-payer loss of access; and old-order permission
with denied proposed-payer authority. Demonstrate that allowed proposed values are the values
actually persisted and denied proposals produce no business changes. Verify audit-read separation,
Workflow execution restrictions, event-consumer read restrictions for the Workflow, Subscriptions
and Billing consumer principals, and fail-closed outage behavior. Orders owns the integration tests
and correct request/scope handling; platform owners own provider policy behavior and provisioning.

Workflow and event-consumer read restrictions mean explicit finite order-ID grants in **every**
alternative returned scope — for Workflow's execution operations and for the `order × read` grants
of the Workflow, Subscriptions and Billing event-consumer principals ([08 §4.3](DESIGN.md#contract-08-4-3) *Event-consumer read path*) — enforced using the existing toolkit ID mapping/constraint representation. Verify how the
selected provider provisions, updates and revokes these grants; an execution-correlation string
alone proves no relationship and cannot grant access. Orders keeps three explicit tenant
properties with `no_tenant`, not an invented designated owner. The platform-root broker tenant
is an internal stream boundary (Foundation §4.4), never a substitute for these business scopes.

**Status:** open production-integration verification, not proof that a new platform feature is
required. Reuse existing provider and toolkit capabilities first; propose an extension only after
a concrete unmet requirement is demonstrated. The inspected static and tenant-resolver plugins
do not establish this complete policy, and Pricing's integration pattern does not establish
deployment support either. Record the responsible platform owner and evidence before production
use of affected operations. No external owner agreement, deployed-provider verification or
mandatory new proposed-value validation API is implied by this requirement. The delegation-proof
items above are the one concrete unmet requirement already demonstrated: the SDK has no request
carrier for caller evidence and no agreed proof deny codes.

### 2.10 Catalog registry (Product & SKU)

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-catalog-subscription-product-key`

The catalog registry (`products`, "Catalog registry (Product & SKU)" in the Subscriptions naming
of `SUB-N2`) **MUST** expose, through its SDK, each plan/SKU's **`catalogSubscriptionProductKey`**
— the registry-owned stable key for the sellable subscription product/family — **at a given
committed `CatalogVersion`**, batched for a whole basket (up to 200 lines) in one call. The answer
must distinguish a line the registry maps to no key from an outage, so Orders can refuse
`overlap-key-unresolvable` for the first and `catalog-product-key-unavailable` for the second.
This is the key Subscriptions binds its `overlapScopeKey` default
`(payerTenantId, catalogSubscriptionProductKey)` to under **`SUB-G1`**, whose shape is being agreed on
**PR #4177**; Orders adopts that shape and does not define its own. The submit gate resolves it at
the run's fixed catalog version ([03 §3.6](features/03-gate-and-pin.md#contract-03-3-6) *Run Gate and Submit* step 4),
persists it as `orders_order_line.overlap_scope_key`, and claims it for the payer under the
in-flight index. Until the operation exists every submit or amendment refuses with
`catalog-product-key-unavailable` (fail closed, ADR-0003), because an overlap key derived locally
from line fields would let Orders and Subscriptions disagree about which orders collide. See
[`DECISIONS.md`](./DECISIONS.md) D-108.

### 2.11 Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-contract-party-eligibility`

**Contract status and party eligibility for a referenced contract.** Where an order references a
`contractId`, the gate's contract-resolution port — the only port answering party eligibility —
needs, through a Contracts SDK, one batched read returning the contract's status (active or not)
and whether the payer is party-eligible to purchase the basket's catalog scope under it, with a
machine-readable business reason on a negative answer. This is the predicate the Contracts PRD
already names (Contracts PRD §6.6 *Party eligibility predicate*, consumed by the order-capture gate).
Orders maps an inactive contract to `contract-not-active`, an ineligible party to
`contract-party-ineligible`, and outage or an unimplemented operation to
`contract-resolution-unavailable` (fail closed, ADR-0003). The gear has a PRD and no
implementation or SDK today ([03 §3.5](DESIGN.md#contract-03-3-5), §4.2 predicate 2).

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-contract-acceptance-declaration`

**The contract's acceptance-required declaration.** The same contract-resolution read must also
return, where an order references a `contractId`, whether customer acceptance is required for
services sold under that contract (`acceptance_required`). This is the declaration the Contracts PRD
already requires the contract to carry (Contracts PRD §6.6 *Booking instant and acceptance*: the contract
"**MUST** declare whether customer acceptance is required"). Orders reads it live — not snapshotted
at submit — at the acceptance-recording and begin-fulfillment guards, where it outranks the seller
and platform elections ([05 §3.5](DESIGN.md#contract-05-3-5), §4.1, D-107, D-132). Until the SDK
exists, a contract-referenced order resolves `acceptance-requirement-unevaluable` (fail closed,
ADR-0003) at both guards; it never falls back to an election.

## 3. Priorities

| Priority | Requirements |
|----------|-------------|
| `p1` (critical) | `…-upreq-subscription-start-instant`, `…-upreq-overlap-presence-read`, `…-upreq-compensation-cancel-reason`, `…-upreq-pre-subscription-evaluation`, `…-upreq-tcv-with-annualisation`, `…-upreq-external-reference-propagation`, `…-upreq-delegation-proof-credential`, `…-upreq-authorization-outcome`, `…-upreq-workflow-amendment-verdict`, `…-upreq-event-broker-runtime`, `…-upreq-event-broker-cursor-retry`, `…-upreq-event-broker-dead-letter-recovery`, `…-upreq-event-broker-root-tenancy`, `…-upreq-event-delivery-observability`, `…-upreq-catalog-subscription-product-key`, `…-upreq-pricing-catalog-tenant-reads`, `…-upreq-rating-evaluation`, `…-upreq-payer-commercial-profile`, `…-upreq-contract-party-eligibility`, `…-upreq-contract-acceptance-declaration` |
| `p2` (important) | `…-upreq-order-reference-on-create`, `…-upreq-two-phase-pair-preserved`, `…-upreq-correlation-propagation`, `…-upreq-indicative-tax-read`, `…-upreq-audit-identity-lifecycle` |

`cpt-cf-bss-orders-lifecycle-upreq-pdp-policy-integration` is also `p1`: verification and
provisioning of platform authorization are required for production caller-driven access, and it
includes PDP evaluation of delegation proof supplied as request context, with distinct
missing/invalid deny reasons (D-111).

Two asks are cheaper now than later for structural reasons rather than scheduling ones. The
compensation cancel reason rides event payloads that downstream consumers key on. The start
instant determines whether a deferred line's subscription can ever be correct, and no order-side
mitigation exists.

## 4. Traceability

- **PRD**: [`./PRD.md`](./PRD.md) — §13 dependencies, §15 open questions
- **DESIGN**: [`./DESIGN.md`](./DESIGN.md) §3.5, §3.8; [01 §3.8](DESIGN.md#contract-01-3-8), §4.4; [03 §2.2](DESIGN.md#contract-03-2-2); [06 §4.2](DESIGN.md#contract-06-4-2), §4.6
- **Decisions**: [`./DECISIONS.md`](./DECISIONS.md) — D-32, D-56, D-108, D-111, D-122, D-124, Q-04, Q-05, Q-08
- **ADRs**: [`./ADR/0003`](./ADR/0003-cpt-cf-bss-orders-lifecycle-adr-fail-closed-gate.md) — the fail-closed posture that makes `SUB-O5` a blocker rather than a degradation; [`./ADR/0006`](./ADR/0006-cpt-cf-bss-orders-lifecycle-adr-outbox-publication.md) — the platform producer path and Event Broker readiness gate
- **Upstream registers**: `gears/bss/subscriptions/docs/SEAMS.md` §I (`SUB-O1`…`SUB-O6`); the sibling Workflow PRD §13 (`SUB-O5`…`SUB-O9`); `gears/bss/rating/docs/SEAMS.md` for the three Rating asks; `gears/bss/contracts/docs/PRD.md` §6.6 (*Party eligibility predicate*, *Booking instant and acceptance*) for the Contracts asks; `gears/bss/subscriptions/docs/SEAMS.md` `SUB-G1` (PR #4177) for the catalog-registry product key. Rating and the billing chain **are** specified in this repository — Rating carries a PRD, a DESIGN, ADRs and its own seam register, and the billing chain is specified as `gears/bss/ledger` — so both asks must be raised against those specifications rather than treated as unowned. **Payments alone has no specification and no register**, which is why that one ask is recorded here for whichever specification takes it; the same applies to a distinct tax owner, which `ledger` does not claim to be
