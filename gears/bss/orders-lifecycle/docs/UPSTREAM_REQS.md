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
  - [2.12 API Gateway](#212-api-gateway)
- [3. Priorities](#3-priorities)
- [4. Traceability](#4-traceability)
- [PriceBook readiness additions](#pricebook-readiness-additions)

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
| `orders-lifecycle` | Owns the order document and its state machine; needs Subscriptions to accept an explicit start instant, expose an overlap-occupancy read, and carry an order reference and a compensation cancellation reason. Needs Pricing to publish its existing reads as `PricingReadV1` and to admit a `bss-orders.system` subject with `plan:read` and `price:read`, as it does for Rating and Subscriptions; the residual purchase verdict is a separate, narrower ask. Needs Rating to expose a batched exact-binding evaluation SDK, a pre-subscription evaluation and an annualised TCV figure. Needs the billing chain to propagate the external reference and to answer an indicative tax read. Needs Account Management to issue verifiable delegation proof and expose the payer's commercial profile, Contracts to answer contract status, party eligibility and the acceptance-required declaration for a referenced contract, and the platform PDP to evaluate it from request context with distinct missing/invalid deny reasons. Needs Subscriptions to answer its `SUB-G1` overlap key for a prospective PriceBook line, and Pricing's revision-reference release report to count in-flight orders; SKU protection itself is inherited from the revision's references. Needs a Payments capability that does not exist, and needs the Event Broker runtime behind the
landed SDK before event-producing traffic can be accepted. Needs the API Gateway to key a rate-limit zone by a path parameter so the per-(caller, order) request limit can run at the gateway (D-185). |
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

The Workflow branch (`bss/orders-workflow` @ `3ccf7793c`) registers the provisioning-intent contract
on its own side as `SUB-O11`…`SUB-O16`; the asks below that overlap them name the matching Workflow
id so Subscriptions receives one list, not two (DECISIONS D-172–D-175).

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-subscription-start-instant`

The activation intent and `create` **MUST** accept an explicit **start instant** and **MUST NOT**
derive the subscription start from any date carried on the order. Where the two-phase activation
barrier defers a line past its quoted service-activation date, the spawned subscription's start is
the **actual activation instant**; billing and entitlement **MUST NOT** be backdated to the
earlier quoted date. Raised as **`SUB-O10`** — a new ask, not present in either existing register.
Without it the PRD's no-backdating requirement is unenforceable from the order side, because
Subscriptions owns the start. See [`DECISIONS.md`](./DECISIONS.md) D-56. The Workflow branch carries
the instant on its activate intent (Workflow D-195); `start_at` wins over any internally computed
instant.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-overlap-presence-read`

An **overlap-key occupancy read**: batched, and for each `(payer_tenant_id, resource_tenant_id, overlap_scope_key)`
returning `(activeCount, maxConcurrentActive, provenance)` — the number of subscriptions `active`
on that key (drafts excluded), the effective concurrent-active cardinality, and the Catalog/Contract
policy that cardinality was resolved from. Registered upstream as **`SUB-O5`**, unagreed.
**This is an amendment to `SUB-O5`**, whose upstream text asks for presence ("this gear answers
presence"): a boolean cannot evaluate `activeCount + proposed ≤ maxConcurrentActive` when the limit
exceeds one, so this design asks for the count and the limit; the Subscriptions gear's own seam map
is not edited here ([`DECISIONS.md`](./DECISIONS.md) D-126). The requirement ID is kept for
stability. **Second amendment (D-179; open as Q-40):** the tuple carries the resource tenant, by making
`resourceTenantId` a default dimension of `overlapScopeKey` — Subscriptions' own
`design/03-plan-changes.md` §4.4 already permits extra dimensions — and enforcing the same tuple at
the active commit. On self-service sales payer and resource tenant are one tenant, so only the
partner path changes. Until Subscriptions enforces the resource dimension at its active commit, it answers on the tuple it
enforces and says so in `provenance`; predicate 7 applies that answer as given, so a per-payer answer
refuses a partner's second customer at cardinality one at submit when that customer's subscription
is already `active`, and — because predicate 7's `proposed` also counts the lines of this payer's
other in-flight orders claiming the same key under another resource tenant (D-180) — when two such
orders are in flight at once, rather than passing either into an activation refusal. Orders never
re-buckets a per-payer count locally (D-83: no local fork); the addend is its own claim data. The against-existing-subscriptions half of the
submit gate's overlap predicate depends on it; until it lands that half is unevaluable and
therefore a refusal, which fails closed. The same read serves Workflow's pre-wave-2 re-check
(Workflow D-195); the shape is `occupancy(payer, resource_tenant, keys[]) → [{ key, active_count, draft_count,
max_concurrent_active, source }]`, with the keys obtained through
`…-upreq-catalog-subscription-product-key`.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-compensation-cancel-reason`

A cancellation **reason value for order-fulfillment compensation**, scoped **out** of the
early-termination class so it derives neither a termination fee nor a credit. Registered upstream
as **`SUB-O1`**, marked critical there, unagreed. The upstream note records that reason values
ride event payloads consumers key on, so adding one after Billing consumes the contract is a
breaking change — making this the ask materially cheaper now than later. Workflow submits both compensation legs
(draft void, activated cancel) as provisioning intents with reason `order_compensation`;
Subscriptions treats void as `cancel` from draft (SUB-D-11), so one
`CancelReason::OrderCompensation { order_id, order_version }` accepted from draft and from active
closes both legs.

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

**Release gate (D-180).** This ask **MUST** be agreed with Subscriptions, scheduled and delivered
before the submit/activation path is production-ready; until then subscription-side cardinality is
**advisory at order time** and the gate contract and consumer documents say so. What Orders
contributes is bounded: at most one in-flight order per claim tuple (D-179), plus predicate 7's
interim count of this payer's other in-flight orders on the same key while the occupancy answer is
per payer. The residual race is with entries into `active` that bypass Orders — direct
subscriptions, `resume`, `transfer`, key-altering `changePlan` — which only Subscriptions can close.

**Mechanism proposed to Subscriptions.** *Recommended*: mirror this gear's
[ADR-0007](./ADR/0007-cpt-cf-bss-orders-lifecycle-adr-in-transaction-concurrency.md) — an
in-transaction **slot claim** `(overlapScopeKey, slot)` with a partial UNIQUE over live claims and
a row CHECK `0 ≤ slot < maxConcurrentActive` (the limit resolved for that key), taken in the same
transaction that commits `active` and released in the transaction that leaves `active`; a
transaction that finds no free slot commits nothing and returns `overlap-collision`. Every entry
into `active` that §4.4 of Subscriptions' `design/03-plan-changes.md` already detects on takes a
slot, so the rule binds all writers, not only Orders'; how the §4.4 supersedes exemption maps onto
slots is Subscriptions' design. The slot shape D-83 rejected for Orders is the right shape here:
Orders' in-flight cap is fixed at one by PRD §6.1(g), whereas Subscriptions' cardinality is
configurable by PRD §6.1(f), so "at most N" is the rule itself, not unused schema surface.
*Alternative*: lock one per-key row (`SELECT … FOR UPDATE`) inside that transaction, then count
and commit. *Rejected*: a `gears/bss/libs/coord` lease — its README, "Don't use `coord` when…",
excludes hard mutual exclusion with zero tolerance for a TTL-expiry overlap, and the lease is
TTL-based and not scoped to the committing transaction; toolkit-db advisory locks
(`libs/toolkit-db/src/advisory_locks.rs`) — session-level `pg_try_advisory_lock` / `GET_LOCK` on a
pinned connection, not transaction-scoped, and broken by a transaction-pooling proxy. The
Subscriptions gear's own documents are not edited here (D-126).

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-settle-create`

**Settlement of a timed-out create.** `settle_create(create_key) → NoDraft { tombstone_id } |
DraftFound { subscription_id, status }`, serialized with `create` on the same unique dedup index so
exactly one of a late create and a settlement wins. Needed on the F12 compensation path: a status
read alone cannot prove that a delayed create will not commit later and attach a subscription to a
settled order. Co-registered with the Workflow branch's `SUB-O13` (unasked upstream); Subscriptions
slice 01 has only the dedup key today. See [`DECISIONS.md`](./DECISIONS.md) D-172.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-intent-status-read`

**Status of a non-terminal provisioning intent**, by transition request id or by the full
`(tenant, order, version, line, wave, kind[, attempt])` tuple: `approved | applied |
oss_unconfirmed | failed`, with the subscription id where one exists. Workflow's reconcile step
detects by re-read, never by absence of notification; Lifecycle counts a line as activated only at
`applied` (D-165). The Workflow branch's `SUB-O13`; not in Subscriptions' register.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-transition-outcome-echo`

**One outcome event per intent, echoing the caller's identity.** `SubscriptionTransitionOutcome {
transition_request_id, source { order_id, order_version, order_line_id }, wave, kind, wave_attempt,
correlation_id, transition, outcome: applied | failed { reason_code } | oss_unconfirmed }` for
create, activate, void and cancel, as the twin of `SubscriptionActivated`, which carries no caller
tuple and has no failure variant. Without it Workflow cannot correlate the event Subscriptions would
send and falls back to the status read for every outcome; Lifecycle's completion acknowledgement
depends on Workflow receiving a terminal outcome per line. The Workflow branch's `SUB-O16`; Seam
Atlas names the same event in C03/C09 (ticket T12).

**PriceBook provisioning amendment (D-157, D-165).** The create/activate SDK must accept the exact
order/version/line reference (which is the accepted version reference), the order/line external
reference for the billable facts (§2.3), the Lifecycle acceptance instant, the referenced
`contractId`, explicit actual start intent and tenant axes. Item composition is not passed:
Subscriptions reads it, with the accepted `chains[]`, through the authorized Lifecycle version read,
so there is one source of truth, and compares its pinned activation-date resolve against it before
committing `active` (`…-upreq-initial-binding-acceptance`); arbitrary caller-provided monetary
evidence is not accepted. The mapping onto Subscriptions' three existing
instants is fixed: Lifecycle's recorded acceptance instant becomes `customerAcceptedAt`, with the
Lifecycle acceptance record as provenance; the actual activation instant becomes
`serviceActivatedAt` (`SUB-O10`); `contractEffectiveAt` comes from the referenced Contract, never
from an order date. A line counts as activated only when its `activate` transition reaches
`applied`; `approved` with the OSS confirmation pending is not completion, and `oss_unconfirmed`
is a provisioning failure. Preserve Workflow's tenant/order/version/line/wave/kind/rebuild-attempt
idempotency identity inside Subscriptions' opaque `(orderingTenantId, create, client key)` scope; a
line-ID-only key loses amendments and rebuilt drafts.
SUB-O1 adds `order_compensation` outside fee/credit-generating early termination; SUB-O2 carries reverse
order provenance. SUB-O5 must mirror active counts (drafts excluded), effective limit and provenance.
The existing Subscriptions seam numbers remain canonical; SUB-O9/O10 remain explicitly unregistered
extensions until the counterpart records them. No change to the order's separate one-in-flight cap.

### 2.2 Rating / price evaluation

**PriceBook baseline (D-150–D-158).** The required contracts below target Pricing/Products
`16705a243`, Rating T-D-37/T-D-38 and Subscriptions SUB-D-29. Current Pricing REST resolve is
not a purchase verdict, quote, initial-price hold or seller-scoped consumer SDK. Existing IDs are
retained where the obligation survives; scope changes below supersede the old model explicitly.
Each requirement below states the proposed producer-side shape, timing and acceptance cases. No
counterpart acceptance is implied.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pricing-read-sdk`

Pricing must publish `PricingReadV1 { resolve, price, current_revision }` in `pricing-sdk`,
registered in the ClientHub by the Pricing gear: the existing `GET /resolve`, `GET /prices/{id}` and
`GET /plans/{id}` answers with their DTO field names unchanged (`PricingResolveDto` and its item,
chain and binding members; `PricingPinnedPriceDto`; the plan's `published_rev`, revision `state` and
`available_from`). Signatures as the reads exist: `resolve(plan_revision_id, date, item_id?, pins[])`,
`price(price_id)`, `current_revision(plan_id)`; the plan read's `published_rev` is a revision
number, and the revision's `state` and `available_from` are read from its `revisions[]` entry.
The 15 golden contract files freeze `resolve` and `price`; `current_revision` needs its own.
`pricing-sdk` exports only `product_catalog` today, and REST does not count between gears, so this
is the same prerequisite Rating and Subscriptions have; the tenant-scoped subject context of
D-424 (Pricing's `reference_ticker::system_actor` pattern) is only buildable in-process, which
makes the trait a prerequisite for the access model too. Orders asks for nothing beyond those
three reads.
Six of the nine catalog predicates are evaluated from them ([DESIGN §4.1](DESIGN.md#contract-03-4-1),
D-161). A missing revision is distinct from denied/unavailable/malformed answers.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pricing-catalog-tenant-reads`

**Revised scope; stable ID (D-160).** Pricing must admit a `bss-orders.system` system subject with
`plan:read` and `price:read`, recorded beside `bss-rating.system` and `bss-subscriptions.system`
in Pricing D-424 and its PRD actor list. Orders' adapter builds that subject's context for the
order's **seller tenant** (Pricing's `reference_ticker::system_actor` pattern) and calls
`PricingReadV1` in it, exactly as the two existing consumers read a seller's revision; the revision
is looked up in the subject's tenant, and resolve reads SKU versions as Pricing's own actor. The
`sellable`/`lifecycle` row of DESIGN §4.1 is a scoped Products read and needs its own grant
(`…-upreq-products-sku-read-grant`, §2.10). No new API parameter for the seller, no delegation
proof presented to Pricing, no caller-tenant or latest-price fallback. Published/superseded
historical readability does not imply new-purchase eligibility.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pricing-purchase-assessment`

**Narrowed scope; stable ID (D-161).** Only the predicates no existing read can answer remain
here: the market applicability of a selected dimension value to the payer's `(currency, region)`,
and any purchase-eligibility rule the catalog owner holds beyond revision currentness, availability,
membership, chain coverage and the two Products fields. Proposed as `SellabilityV1::check` over
prospective-purchase inputs (revision, date, selected items and dimension values), owned by Pricing
or composed by an owner Pricing names (Atlas decision 1). Until it exists those rows are
`catalog-predicate-unevaluable`; the six read rows are not blocked by it. A small companion ask:
resolve may echo `Sku.sellable` and `Sku.lifecycle` per item so the gate needs no second call.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-initial-binding-acceptance`

**Revised scope; stable ID (D-162).** No new Pricing resolve mode is asked for. Before committing
`active`, Subscriptions reads the accepted matrix of the order version through Lifecycle's
authorized `get_version`, encodes each consumed slot (the slot whose `dim_value` equals the item's
`selected_dim_value`) as a pin under DESIGN §4.3's own-chain/default rule, runs the ordinary
`resolve(plan_revision_id, activation date, pins)` and compares: for every consumed slot the
answered `binding.price_id` equals the accepted one. Pricing's renewal walk keeps the accepted price
while it is in force, stops before a `new` successor and moves only for an `all` successor or an
ended price, so the comparison refuses exactly when the promise cannot be kept. Equal activates on
those bindings, with `pinned_from` as provenance, and stores them as the first period's pins;
different, or a consumed slot missing, refuses with the closed reason `accepted-price-mismatch`,
which Workflow maps to `order-binding-expired` and compensates. A signup resolve is not used.
Replay of a committed activation precedes the comparison. The comparison resolve runs at the
`applied` commit; its date is the first period's start, or Rating reads the stored `price_id`s for
that period through `price`. Subscriptions' `create` gate on a published plan (SUB-P5) must admit
an accepted revision superseded after submit; resolve still answers it. `activation_deadline` is
derived by Orders from the stored bindings and a seller-scoped Orders setting; it is an early check
at submit and before each dispatch, never the admission authority, so no clock agreement between
gears is required. Subscriptions and Rating still own which of them cuts a period at an `ends_on`
inside it (Atlas decision 7).

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-rating-evaluation`

Rating must publish a ClientHub SDK for one logical batched pre-purchase evaluation over the exact
selected bindings of an assessment. The request is the accepted matrix in resolve's own vocabulary,
`lines[{ line_id, plan_revision_id, items[{ item_id, quantity, chains[{ dim_value, binding }] }] }]`
with `assessment_id` and `resolve_date` (D-167), so Rating rates the same object Subscriptions will
store; carry seller/resource/payer context, contract, market and term/cycle beside it. Return
assessment identity, per-item/per-line and whole-order figures, gross/net/discount, promotion
status, currency/scale/rounding, three charge kinds and TCV.
Exact-price mode must not feed order pins to renewal resolve and walk to successors. One-time
preview does not synthesize a one-time recurring/usage rating unit. Minimum fees are Rating's.
Absent SDK, inconsistent binding identity or incomplete required figures is `evaluation-unavailable`.
Every submit/amendment requires totals; Preview retains only the explicitly allowed TCV withholding.
This D-167 request is the single evaluation DTO: Seam Atlas C06 (`acceptance_ref` plus digest,
`ExactAmount` outputs) is not consumed, figures are integer minor units stored verbatim, and
Rating's own `fr-pre-purchase-evaluation` must be promoted from p2 and lose its catalog-version
prefix before the seam exists (Atlas ticket T3).

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pricing-bundle-sellability`

**Narrowed scope; stable ID (D-161).** The immutable item composition, treatments, included
allowances and chain coverage are what resolve already returns and are consumed through
`…-upreq-pricing-read-sdk`; this ID keeps only what that read does not settle: resolve must return
the complete roster or an explicit unavailable answer, never a truncated matrix, and the
composition-only SKU sellability rule (a SKU sold only inside a plan) belongs to the residual
`SellabilityV1` verdict of `…-upreq-pricing-purchase-assessment`. Deferred sold-as/grants and
removed bundle price-basis rules are not implicit prerequisites or functionality. A truncated or
missing roster remains fail-closed as `catalog-predicates-unavailable`, never a passing aggregate
substituted for missing facts.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pre-subscription-evaluation`

No subscription exists at assessment time. Rating must state which contract/market scopes can be
evaluated from the purchase alone, without an activated subscription. Unsupported brand/subscription
context, deferred promotions and indicative tax are explicitly excluded or declared unavailable
according to the shared contract; missing evaluation is never represented as a zero discount.
A supported no-promotion result may explicitly return zero discount with no promotion reference.
Read/Preview surfaces preserve these statuses and exclusions.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-tcv-with-annualisation`

Rating computes whole-order **net pre-tax TCV**, excluding usage, counting one-time once and
annualizing rolling recurring terms by their cycle (12 for `month`, 1 for `year`, the two periods
PriceBook supports).
Orders stores integer minor-unit figures verbatim; it neither sums lines nor converts decimal money.
Rating's current caller-summation wording must be amended. Define mixed recurring cycles and
per-cycle display breakdowns without adding unlike-period figures. For an unsupported cycle/term,
refuse evaluation rather than invent a mapping. Missing Preview term/cycle may withhold only TCV
under the existing explicit annotation; submit/amendment cannot use that exception.

### 2.3 Billing chain

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-external-reference-propagation`

The **external reference** carried onto billing documents. PRD §13 makes it a `MUST` — "External
reference on the order/line **MUST** propagate to billing documents" — for buyer-side
accounts-payable reconciliation. Billing documents derive from the **subscription**, not from order
events, so the route is fixed (D-168): order → Workflow's create envelope → Subscriptions' billable
facts → invoice. Workflow snapshots the order/line external references at the first provisioning
handoff and reuses that snapshot on retries; administrative edits after it affect later handoffs
only. Subscriptions must carry the reference onto its billable facts (§2.1 provisioning amendment),
and the invoicing owner, unowned today (§4), must print it. A purchase-order number that never
reaches the invoice fails the requirement invisibly: the order shows it, the event carries it, and
only the invoice lacks it.

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
[`DECISIONS.md`](./DECISIONS.md) D-128. Shape (2026-10-02): `payer_profile(payer_tenant_id,
seller_tenant_id) → { currency, region, in_relationship_with_seller }`, as a first-class read or a
documented schema on the tenant metadata API; Seam Atlas takes the market from the caller's
request, which this gear treats as untrusted.

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

**Shape and request identity (2026-10-02).** `authorize(request_ref, payer, amount | postpaid) →
Authorized | Pending { request_ref } | Failed { reason }` and `get(request_ref)`; Workflow mints
`request_ref` from its intent key (Workflow Q-06), and the authorized amount is specified
separately from TCV (Workflow D-199). The worktree's `admission-control` gear is platform policy
admission, not this capability; Seam Atlas N4/D05 use the word admission for both, and its
`AdmissionReadV1` is not consumed by this gear (D-175).

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
that result after ambiguous replies. The branch does so (W/design/05:668, :771); its PRD §9.1 still
equates the signal with the first activation intent and must name the call. Workflow re-issues under
one idempotency key for 30 days; Lifecycle's receipt floor for workflow-class triggers is the same
window (D-173). This closes **order** direct cancellation before dispatch;
it is not the Workflow task's potentially later unilateral-cancellation boundary. The Workflow
PRD wording must be reconciled by its owner before release; this document does not claim that
Product has approved the divergence. See [06 §4.3](features/06-workflow-seam.md#contract-06-4-3).

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-workflow-pricebook-contracts`

Workflow must consume the complete Lifecycle SDK (all verdict authorities, explicit completion
line mapping and immutable-version reads), pass the accepted version reference, acceptance
instant, contract reference and external references to Subscriptions (never the bindings
themselves; Subscriptions reads `chains[]` through `get_version`), check `activation_deadline`
before each activation dispatch, treat a line as activated only at Subscriptions' `applied`, map
`oss_unconfirmed` and the receiver's `accepted-price-mismatch` refusal to its failure catalog
(`order-binding-expired` for the latter) and handle receiver refusal through compensation. Its approved-instance
constructor no longer requires a dependency graph: Workflow D-196 (branch `3ccf7793c`) withdrew the
graph and the `dependency-graph-invalid` outcome, and wave ordering is Workflow-internal; the
former unknown-topology clause is withdrawn with it (D-172). Workflow's approval-policy
adapter owns requirement/routing decisions and receives required TCV; embedding `cf-gears-bss-approval`
is an implementation option, not evidence the policy service exists. Payment authorization amount
and provenance must be specified separately from TCV and Ledger settlement. See reciprocal amendments.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-workflow-overdue-escalation`

**Overdue fulfillment escalation with a named owner (D-182).** Orders Workflow **MUST** raise the
PRD §6.3 overdue escalation for every order in `in_fulfillment`, or `on_hold` with pre-hold
`in_fulfillment`, once database time passes expected fulfillment time — `max(begin-fulfillment
instant, latest line service-activation date)` — plus the configurable overdue window (business
default 24 hours), routed to the fulfillment operator (`cpt-cf-bss-orders-workflow-actor-owf-fulfillment-operator`)
as the named owner, durably, once per order and window, with order, version, age and the
unreconciled lines. The same escalation covers a stalled `active → cancelled` compensation
(D-165). It **MUST NOT** auto-terminal the order.

On consuming `OrderFulfillmentFailed` with `failure_reason = operator-forced-unreconciled`, Workflow
**MUST** terminate the process — cease pending provisioning intents and timers — **without**
treating the order as compensated: the event's evidence carries
`no_active_subscription_remains = unknown`. It **MUST** keep, or open, the orphan-subscription
manual task for that order until reconciliation through Subscriptions establishes that no active
subscription remains, and **MUST NOT** call Lifecycle again for that order (any call is refused
against the terminal state). Workflow's own principal **MUST NOT** hold the
`order × force-fail-unreconciled` grant.

**This is a production release prerequisite**, as the dead-letter recovery ask is: no deployment
may admit `begin-fulfillment` in production until the escalation, its owner routing and the
forced-failure handling are delivered and tested, together with Lifecycle's own overdue gauge and
alert ([07 §3.8](DESIGN.md#contract-07-3-8)).

**Current gap:** the Workflow PRD's process termination on terminal order events
(`cpt-cf-bss-orders-workflow-fr-owf-terminal-order-events`, `gears/bss/orders-workflow/docs/PRD.md`)
terminates only on `OrderCancelled`, `OrderExpired` and `OrderRejected`, because every other
`fulfillment_failed` is Workflow's own acknowledgement; it has no rule for a terminal it did not
cause, and its "process deadline is the overdue window" row names no escalation owner or
forced-failure handling. Both need a Workflow PRD amendment by its owner; this document does not
edit that gear or represent the amendment as agreed.

**Owners:** Orders Workflow maintainers.

### 2.7 Event Broker

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-event-consumer-conformance`

**Event consumer conformance (D-186).** Workflow, Subscriptions and Billing owners **MUST** each
implement the [event consumer contract](DESIGN.md#contract-01-event-consumer-contract) (Foundation
§4.4: C1 de-duplication by event ID in a consumer-owned processed-event store, C2 reconciliation
through `get_version` and the current-order read before any business effect, C3 unknown-value
tolerance, C4 no reconstruction, C5 durably pending work on an unavailable or denied read) and
**MUST** pass the shared `orders-events` golden corpus specified there against their real handler,
with their declared per-event applicability rule. **Passing the corpus is the integration
sign-off gate** for each of the three; no consumer integration is accepted on a reading of the
contract alone. Precedent: Pricing gates publish-contract sign-off on the joint proration golden
fixture ([`../../pricing/docs/design/06-consumer-contracts.md`](../../pricing/docs/design/06-consumer-contracts.md)
K5), held in [`gears/bss/fixtures`](../../fixtures/README.md). The corpus is specified in this gear
and built with the first consumer integration (Workflow); Orders states the cases, each consumer
owns its evaluator. The platform supplies no consumer-side processed-event store: the Event Broker
consumer contract places de-duplication on the consumer
([`0002-consumer-subscription-lifecycle.md`](../../../system/event-broker/docs/features/0002-consumer-subscription-lifecycle.md) §2.3).

C2 reads need explicit PDP `order × read` grants constrained to each consumer's authorized target
orders; provisioning and verifying them is owned by §2.9
(`cpt-cf-bss-orders-lifecycle-upreq-pdp-policy-integration`). Platform-root event access supplies
neither these grants nor business authorization. The existing Orders read surface is used; no new
endpoint is requested. Q-25's §9.2 half is closed by D-186 in line with PRD §9.2's PB-2026-09-29
amendment — a business-effect consumer always reads before its effect — so Product/Architecture
account for that read load and availability in integration acceptance.

**Owners:** Workflow, Subscriptions and Billing gear owners; Billing is unowned (D-168), so its
obligation is recorded for whichever specification takes it.
**Tracking status:** open; no consumer implementation or corpus run has been recorded.

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
mechanism and an operator interface. Consumer conformance
(`…-upreq-event-consumer-conformance`) makes a gap safe for consumers; it does not relax this gate,
because a parked event — a terminal `OrderCompleted` included — still has to be delivered. Closure **MUST** record their implementation PRs/deployed
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

**Revised scope; legacy ID retained (D-163).** The key stays the one Subscriptions already defines
and keys its cardinality rule on: the registry-owned `catalogSubscriptionProductKey` of `SUB-G1`,
"bound to a published SKU/product key". Products removed the Product entity but keeps SKUs, so the
derivation proposed to Subscriptions for PriceBook is the SKU of the line's paid `recurring`
item(s) as resolve names them; `plan_id` is not proposed, because two plans selling one family
would stop colliding; a line with several paid recurring items yields one key per such SKU. Orders
submits the prospective line/revision, batched and seller-scoped, to a Subscriptions key operation
(proposed `SubscriptionsOverlapKeyV1::keys`, the SUB-P8 shape: the neighbour submits, Subscriptions
answers) and stores the key(s) plus derivation/policy provenance as answered; it never computes the
key. Missing key and unavailable resolver are distinct. Resolve it once per assessment, store it on
the version, and reuse it at activation. The partner/customer dimension (Q-05) is closed for
the Orders in-flight claim by D-179, which keeps `resource_tenant_id` beside the key rather than
inside it; the subscription-side dimension rides the `SUB-O5` amendment above.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-products-sku-read-grant`

Products must admit the `bss-orders.system` subject to `ProductsClient::get_sku` in the seller
tenant (a SKU read grant), so the gate can read `Sku.sellable` and `Sku.lifecycle` for each consumed
SKU (DESIGN §4.1, D-160). `get_sku` is a scoped read: the tenant argument narrows, it never grants.
P-D-222 (fork tip `7d3544156`) refuses the Pricing system actor at every REST door, so this grant
is the only self-service path; the resolve-echo alternative is withdrawn (D-171). The read is
interim: it retires when Pricing publishes `SellabilityV1` (D-177), after which Seam Atlas P9
holds. Until the grant exists the row is `catalog-predicate-unevaluable`.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-sku-protection`

**Narrowed scope; stable ID (D-164).** Protection is inherited: every item of a published or
superseded revision holds a `plan_item` reference on its SKU, Pricing keeps those references until
Subscriptions reports that no subscription pins the revision, plan retirement is deferred (D-410)
and Products refuses retirement with `SKU_REFERENCED` while a reference is live. Orders asks for no
owner, kind, receipt or reserve/confirm/release. The one ask is the release trigger: when Pricing's
release report is implemented, the holders that keep a revision's references alive must include
orders that have accepted the revision and are not terminal, since a subscription draft not yet
created is invisible to Subscriptions' presence read (`SUB-P8`). Either the Subscriptions report
takes Orders' in-flight claims as an input, or Pricing reads them from this gear's authorized read
surface. Forced administrative retirement needs a documented refusal/compensation outcome, not
prevention from here.

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
implementation or SDK today ([03 §3.5](DESIGN.md#contract-03-3-5), §4.2 predicate 2). Shape
(2026-10-02): `resolve_for_order(contract_id, tenant_axes, catalog_scope[plan_id], at) → { status,
party_eligible, reason_code?, acceptance_required, contract_effective_at, version }`, one call
serving this ask, `…-upreq-contract-acceptance-declaration` and the `contractEffectiveAt` the create
envelope carries; Seam Atlas's `check_active` returns none of the last three.

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

### 2.12 API Gateway

- [ ] `p2` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-gateway-path-param-throttle-key`

**A rate-limit zone keyed by authenticated subject plus a path parameter (D-185).** Orders bounds
engine-entering write requests before the engine because every refused attempt writes a durable
audit row (ADR-0005). The per-caller limit already uses the gateway as it is: an identity-keyed
zone bound through `ThrottlingSpec { rate_limit_zone, require_security_context: true }`
(`libs/toolkit/src/api/operation_builder.rs`; `gears/system/api-gateway/src/middleware/throttling.rs`).
The per-(caller, order) limit — 20 per minute per `(subject_id, orderId)` — cannot be expressed:
`KeyType` is `{ Identity, Ip }` and further variants are deferred until a consumer asks
([`docs/arch/throttling/DESIGN.md`](../../../../docs/arch/throttling/DESIGN.md) D1/D2, §4). This is that
consumer. Shape: an additive `KeyConfig` variant composing the subject id with a named route path
parameter (`orderId`), resolved after authentication, bounded by `max_keys` like the existing
variants. The gateway's integer `/s` `RateSpec` cannot express a sustained rate below 1/s (60/min),
so a per-minute unit or a fractional rate is part of the ask. Cross-replica enforcement is
the gateway's own open ADR-0001 and is not asked here.

**Fallback until delivered (Q-26)**: a gear-local limiter at the Orders REST edge keyed
`(subject_id, orderId)`, before the engine call, writing no audit row and answering 429; Orders
removes it when the gateway variant lands. Architecture decides between waiting and the fallback.

## 3. Priorities

| Priority | Requirements |
|----------|-------------|
| `p1` (critical) | `…-upreq-subscription-start-instant`, `…-upreq-overlap-presence-read`, `…-upreq-compensation-cancel-reason`, `…-upreq-pre-subscription-evaluation`, `…-upreq-tcv-with-annualisation`, `…-upreq-external-reference-propagation`, `…-upreq-delegation-proof-credential`, `…-upreq-authorization-outcome`, `…-upreq-event-consumer-conformance`, `…-upreq-workflow-amendment-verdict`, `…-upreq-workflow-overdue-escalation`, `…-upreq-event-broker-runtime`, `…-upreq-event-broker-cursor-retry`, `…-upreq-event-broker-dead-letter-recovery`, `…-upreq-event-broker-root-tenancy`, `…-upreq-event-delivery-observability`, `…-upreq-catalog-subscription-product-key`, `…-upreq-pricing-read-sdk`, `…-upreq-pricing-catalog-tenant-reads`, `…-upreq-products-sku-read-grant`, `…-upreq-pricing-purchase-assessment`, `…-upreq-initial-binding-acceptance`, `…-upreq-sku-protection`, `…-upreq-rating-evaluation`, `…-upreq-payer-commercial-profile`, `…-upreq-contract-party-eligibility`, `…-upreq-contract-acceptance-declaration`, `…-upreq-settle-create`, `…-upreq-intent-status-read`, `…-upreq-transition-outcome-echo`, `…-upreq-overlap-activation-atomicity` (release gate for submit/activation, D-180) |
| `p2` (important) | `…-upreq-order-reference-on-create`, `…-upreq-two-phase-pair-preserved`, `…-upreq-correlation-propagation`, `…-upreq-indicative-tax-read`, `…-upreq-audit-identity-lifecycle`, `…-upreq-gateway-path-param-throttle-key` |

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
- **Decisions**: [`./DECISIONS.md`](./DECISIONS.md) — D-32, D-56, D-108, D-111, D-122, D-124, D-150–D-179, D-182, D-185, D-186, Q-04, Q-05, Q-08, Q-26, Q-32, Q-33
- **ADRs**: [`./ADR/0003`](./ADR/0003-cpt-cf-bss-orders-lifecycle-adr-fail-closed-gate.md) — the fail-closed posture that makes `SUB-O5` a blocker rather than a degradation; [`./ADR/0006`](./ADR/0006-cpt-cf-bss-orders-lifecycle-adr-outbox-publication.md) — the platform producer path and Event Broker readiness gate
- **Upstream registers**: `gears/bss/subscriptions/docs/SEAMS.md` §I (`SUB-O1`…`SUB-O6`); the sibling Workflow PRD §13 (`SUB-O5`…`SUB-O9`); `gears/bss/rating/docs/SEAMS.md` for the three Rating asks; `gears/bss/contracts/docs/PRD.md` §6.6 (*Party eligibility predicate*, *Booking instant and acceptance*) for the Contracts asks; `gears/bss/subscriptions/docs/SEAMS.md` `SUB-G1` (PR #4177) for the catalog-registry product key; `gears/bss/pricing/docs` D-419–D-425 and PRD §2.2 for the Pricing reads and system subjects; `gears/bss/products/docs` P-D-189/P-D-194 for SKU lifecycle and references. Rating **is** specified in this repository, with a PRD, a DESIGN, ADRs and its own seam register, so its asks are raised against that specification.
- **Billing chain ownership (D-168).** The billing chain is **not** `gears/bss/ledger`. The Ledger is built and its `LedgerClientV1` is the GL posting and settlement target (`post_balanced_entry`, `settle_payment`, `allocate_payment`, `return_payment`, `record_dispute_phase`, credit application, AR balances, revenue recognition); it generates no invoices, values no at-sale facts and answers no tax, and settlement is not payment authorization. The capabilities this gear needs are owned as follows:

  | Capability | Owner | Register target |
  |---|---|---|
  | Invoice generation and at-sale valuation, carrying the order/line external reference | Billing/invoicing — **unowned** | `…-upreq-external-reference-propagation`, recorded here for whichever specification takes it |
  | Indicative tax for Preview | Tax capability — **unowned** | `…-upreq-indicative-tax-read`, likewise |
  | Payment authorization and PSP interaction | Payments — **unowned** | `…-upreq-authorization-outcome`, likewise |
  | Balanced postings, settlement, allocation, returns, disputes | `gears/bss/ledger`, built | No Orders ask; the Ledger is reached by the PSP adapter and Billing, neither of which exists |

  The external reference travels order → Workflow create → Subscriptions billable fact → invoice, snapshotted at the first provisioning handoff and reused on retries; administrative edits after that affect later handoffs only.

## PriceBook readiness additions

The p1 requirements `…-upreq-pricing-read-sdk`, `…-upreq-pricing-purchase-assessment` (narrowed to
the residual verdict), `…-upreq-initial-binding-acceptance` (compare-at-activation),
`…-upreq-sku-protection` (narrowed to the release trigger) and `…-upreq-workflow-pricebook-contracts`
are additional release prerequisites. Existing seller-read, Rating, composition and overlap-key IDs
carry the revised contracts above. The 2026-09-30 revision (D-159–D-168) pulls each ask back onto the
seam that already exists where one does; no ID marked unchecked is represented as delivered.

**Atlas overlay alignment (2026-10-02).** Three asks were added to §2.1 (`…-upreq-settle-create`,
`…-upreq-intent-status-read`, `…-upreq-transition-outcome-echo`), co-signed with the Workflow
branch's `SUB-O13`/`SUB-O16`; §2.2, §2.4, §2.5, §2.6, §2.10 and §2.11 carry field shapes and the
fork-tip facts (Pricing D-454/D-460/D-467/D-469, Products P-D-222, Workflow D-193–D-199).
Decisions D-169–D-178 record the Orders-side positions; Q-32 (the hold) and Q-33 (add-ons and
change orders) are the two open product questions.
