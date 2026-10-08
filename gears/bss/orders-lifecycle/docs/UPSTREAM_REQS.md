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
  - [R02 reconciliation: existing durable acceptance contract (D-188)](#r02-reconciliation-existing-durable-acceptance-contract-d-188)
  - [R06 downstream adoption requirement (D-192; not delivered)](#r06-downstream-adoption-requirement-d-192-not-delivered)
  - [R11 exact-binding evaluation delivery (D-197; missing runtime)](#r11-exact-binding-evaluation-delivery-d-197-missing-runtime)
  - [R13 commercial owner readiness (D-199; not delivered)](#r13-commercial-owner-readiness-d-199-not-delivered)

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
| `orders-lifecycle` | Owns the order document and its state machine; needs Subscriptions to accept an explicit start instant, expose an overlap-occupancy read, and carry an order reference and a compensation cancellation reason. Needs Orders to integrate the existing typed `PricingReadV1` through its own adapter (D-187), resolve required evidence gaps, and Pricing to admit an authenticated Orders service principal (D-194) with `plan:read` and `price:read`, with the remaining commercial permissions and provisioning specified by D-194; D-189 nonbinding Pricing assessment is a separate upstream implementation ask. Needs Rating to expose a batched exact-binding evaluation SDK, a pre-subscription evaluation and an annualised TCV figure. Needs the billing chain to propagate the external reference and to answer an indicative tax read. Needs Account Management to issue verifiable delegation proof and expose the payer's commercial profile, Contracts to answer contract status, party eligibility and the acceptance-required declaration for a referenced contract, and the platform PDP to evaluate it from request context with distinct missing/invalid deny reasons. Needs Subscriptions to answer its `SUB-G1` overlap key for a prospective PriceBook line, and Pricing to coordinate fenced Orders/Subscriptions usage before future revision-reference release (D-196); SKU protection itself is inherited from the revision's references. Needs a Payments capability that does not exist, and needs integration with the existing Event Broker runtime, deployed grants and verified producer recovery before event-producing traffic can be accepted (D-200). Needs the API Gateway to key a rate-limit zone by a path parameter so the per-(caller, order) request limit can run at the gateway (D-185). |
| `orders-workflow` | Must consume `OrderAmended`, obtain the approval-requirement verdict for the new order version, and reflect the new version onward from `submitted`; without this the Lifecycle two-step re-approval seam stalls. |

## 2. Requirements

### 2.1 Subscriptions

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pre-subscription-billing-terms`

**D-193, proposed missing producer:** Subscriptions must expose the authorized, side-effect-free
[pre-subscription billing-terms resolver](DESIGN.md#contract-03-billing-terms-resolution).
It supplies complete public Pricing BillingTerms with canonical digest, exact invoice-period
Term and retained source/version evidence without creating a subscription/receipt/hold. Agree
and implement seller-policy source/precedence, immutable reads and grants; Settings is not a
selected backend. Validate month/year UTC anchors, supported selected-binding combinations,
exact capture mapping, Preview incompleteness and delayed activation geometry with Rating.
Existing Pricing types/validation do not deliver this resolver. Orders adapts and freezes its
result through D-188/D-192; exact retries never re-resolve it. Provider, permissions and joint
conformance remain production acceptance prerequisites.


Use the [semantic SUB alias map](#subscription-seam-alias-map) below to resolve cross-document
references. Existing Subscriptions `SUB-O1`…`SUB-O6` identifiers are preserved; a bare number
from another document is not an API identity. In particular Subscriptions `SUB-O6` and Workflow
`SUB-O6` name different obligations. The full `cpt-…-upreq-*` IDs below identify Lifecycle asks.
`SUB-O3` is preservation of the externally callable two-phase pair, not proof of receiver delivery.

Historical references to the Workflow branch (`bss/orders-workflow` @ `3ccf7793c`) retain their
provenance. Its `SUB-O11`…`SUB-O16` labels are not declarations in the checked-in Workflow PRD;
do not generate APIs or merge obligations from those numbers. Both settle-create and status-read
were cited as historical `SUB-O13`, so their distinct semantic identities remain authoritative.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-subscription-start-instant`

Create/activate **MUST** preserve distinct quoted dates, exact immutable held Pricing activation
identity and receiver-recorded intent time. The authoritative applied OSS outcome supplies the
actual service-effective instant; persist it with the applied transition identity, not at draft
creation or intent admission. Billing and entitlement **MUST NOT** precede actual activation.
A future confirmation instant cannot be a required activation request input. D-201 supersedes
D-56/Workflow D-195's ambiguous `start_at wins` wording; use the
[clock/held-terms compatibility contract](DESIGN.md#contract-06-activation-clocks).
The selected BillingTerms/anchor cannot be silently rewritten; unsupported receiver/Rating
profiles require reconciliation or newly accepted execution. Historical **SUB-O10** remains
provenance, not an already registered callable API. Owner implementation/conformance is open.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-overlap-presence-read`

An **overlap-key occupancy read**: batched, and for each `(payer_tenant_id, resource_tenant_id, overlap_scope_key)`
returning `(activeCount, maxConcurrentActive, provenance)` — the number of subscriptions `active`
on that key (drafts excluded), the effective concurrent-active cardinality, and the Catalog/Contract
policy that cardinality was resolved from. Registered upstream as **`SUB-O5`**, unagreed.
**This is an amendment to `SUB-O5`**, whose upstream text asks for presence ("this gear answers
presence"): a boolean cannot evaluate `activeCount + proposed ≤ maxConcurrentActive` when the limit
exceeds one, so this design asks for the count and the limit; the Subscriptions gear's own seam map
is not edited here ([`DECISIONS.md`](./DECISIONS.md) D-126). The requirement ID is kept for
stability. **Second amendment (D-179; Q-40 target selected by D-198, producer adoption pending):** the tuple carries the resource tenant, by making
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

**Atomic enforcement of `overlapScopeKey` from activation-intent admission through active confirmation (D-198).** The [receiver protocol](DESIGN.md#contract-06-activation-admission) adds durable pending capacity, exact committed-order/Workflow-generation fencing and staged pause/revoke barriers. The scope includes resource tenant as the selected target; legacy answers cannot be locally re-bucketed. Provider delivery remains unchecked. The
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
a row CHECK `0 ≤ slot < maxConcurrentActive` (the limit resolved for that key, under the authoritative key-policy-generation lock), taken atomically with activation-intent admission before OSS dispatch, retained through pending provisioning and the `active` commit, and released only on definitively settled abort or leaving `active` (D-198); a
transaction that finds no free slot commits nothing and returns `overlap-collision`. Both activation-intent admission and active confirmation validate the same receiver generation; pause/revoke serializes on that receiver row. Every entry
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
`contractId`, distinct quoted/held activation inputs and tenant axes (D-201). Item composition is not passed:
Subscriptions reads the complete D-192 selected receipt/query/bindings through the authorized Lifecycle version read, then matches its hold and fresh eligibility under D-190 before fenced activation admission (`…-upreq-initial-binding-acceptance`); arbitrary caller-provided monetary
evidence is not accepted. The mapping onto Subscriptions' three existing
instants is fixed: Lifecycle's recorded acceptance instant becomes `customerAcceptedAt`, with the
Lifecycle acceptance record as provenance; the authoritative applied service-effective instant becomes
`serviceActivatedAt` (`SUB-O10`); `contractEffectiveAt` comes from the referenced Contract, never
from an order date. A line counts as activated only when its `activate` transition reaches
`applied`; `approved` with the OSS confirmation pending is not completion, and `oss_unconfirmed`
is a provisioning failure. Preserve Workflow's tenant/order/version/line/wave/kind/rebuild-attempt
idempotency identity inside Subscriptions' opaque `(orderingTenantId, create, client key)` scope; a
line-ID-only key loses amendments and rebuilt drafts.
SUB-O1 adds `order_compensation` outside fee/credit-generating early termination; SUB-O2 carries reverse
order provenance. SUB-O5 must expose active and admitted-pending occupancy separately, effective limit and provenance under D-198. Draft existence alone consumes no activation slot; an admitted activation awaiting OSS confirmation does. The selected key includes resource tenant, and Orders cannot locally re-bucket an older provider answer.
The existing Subscriptions seam numbers remain canonical; SUB-O9/O10 remain explicitly unregistered
extensions until the counterpart records them. No change to the order's separate one-in-flight cap.


<a id="subscription-seam-alias-map"></a>

#### Canonical semantic SUB alias map (S1-01)

Resolve references by **source + meaning**, not bare number. Existing requirement IDs are
preserved. This is a documentation mapping, not a new SDK surface or evidence of provider
implementation. The matching public SDK request/result/fence details are S1-05/S5-01 work.

| Semantic contract / canonical Lifecycle requirement suffix | Subscriptions SEAMS §I | Checked-in Workflow PRD | Historical branch alias / disposition |
|---|---|---|---|
| Compensation cancellation reason / `compensation-cancel-reason` | SUB-O1 | Compensation contract | Preserve reason outside early-termination fee/credit class |
| Reverse order provenance and create key / `order-reference-on-create` | SUB-O2 | §6.3 intent identity | D-198 adds exact attempt/generation/roster; do not derive a line-only key |
| Externally callable draft/activate pair / `two-phase-pair-preserved` | SUB-O3 | §6.3 two-wave fulfillment | Preservation ask, with receiver implementation still required |
| Nonbinding pre-purchase conjunction / `pricing-purchase-assessment` | SUB-O4 | Sellability re-check expectations | D-189 Pricing owner; not a Subscriptions acceptance command |
| Canonical key and occupancy / `overlap-presence-read`, `catalog-subscription-product-key` | SUB-O5 plus registry SUB-G1 | SUB-O5 | D-198 resource-tenant key, active plus admitted-pending capacity |
| Atomic multi-subscription orchestration discussion | SUB-O6 | No same-number equivalence | No new bulk API selected; Workflow two waves and per-receiver D-198 admission |
| In-flight duplicate rejection | Not SUB-O6 | SUB-O6 | Receiver intent contract in S5-05; separate from atomic multi-subscription discussion |
| Cancel/void an accepted transition request | Not registered as SUB-O7 | SUB-O7 | Receiver control/compensation S5-05/09; not a blind duplicate submit |
| Intent status / `intent-status-read` | Not registered as SUB-O8 | SUB-O8 | Historical branch SUB-O13; use semantic identity |
| Correlation propagation / `correlation-propagation` | Not registered | SUB-O9 | Process correlation remains distinct from idempotency identity |
| Actual subscription start / `subscription-start-instant` | Not registered | Actual-start requirement | Lifecycle-raised SUB-O10; D-201 reconciled clocks, owner implementation pending |
| Late-create settlement / `settle-create` | Not registered | No numbered equivalent | Also historically cited as SUB-O13; distinct from status-read and serialized against late create |
| Terminal identity echo / `transition-outcome-echo` | Not registered | §6.3/FR intent identity echo | Historical branch SUB-O16; distinguish terminal applied from intent approved |
| Remaining historical SUB-O11/12/14/15 references | No declaration verified | No declaration in checked-in PRD | Retain provenance only; no guessed alias or generated provider method |

Subscriptions' older SEAMS occupancy wording is narrowed by its selected D-198 design amendment:
unadmitted drafts consume no slot; admitted activation awaiting confirmation does. Owner adoption,
all-writer enforcement and deployment conformance remain unchecked.

**R06 downstream adoption (D-192; not delivered).** Registered here because Subscriptions is the
first adopter; the [R06 section](#r06-downstream-adoption-requirement-d-192-not-delivered) points here.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-frozen-commercial-materialization`

Subscriptions/Rating must adopt the [D-192 first-period mapping](DESIGN.md#contract-03-frozen-commercial-snapshot): complete selected receipt query (including BillingTerms) plus identical HeldBindings, immutable receipt/hold provenance and receiver-owned activation/component facts. Hold alone is incomplete. Subscriptions contributes its segment; Rating owns broader snapshot composition and any required additional authoritative inputs. No mutable SKU/meter/invoice refresh, invented legacy provenance or assumed overlay/tax/FX policy is permitted. Implement scoped reads, versioned lossless wire mapping and joint round-trip/change/replay/negative conformance before activation delivery. Existing Pricing bindings/hold behavior satisfy the producer freeze, not downstream integration. R12 remains separately blocking.

### 2.2 Rating / price evaluation

**PriceBook baseline (D-150–D-158).** The required contracts below target Pricing/Products
`16705a243`, Rating T-D-37/T-D-38 and Subscriptions SUB-D-29. Current Pricing REST resolve is
not a purchase verdict, quote, initial-price hold or seller-scoped consumer SDK. Existing IDs are
retained where the obligation survives; scope changes below supersede the old model explicitly.
Each requirement below states the proposed producer-side shape, timing and acceptance cases. No
counterpart acceptance is implied.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pricing-read-sdk`

**Reconciled scope; stable ID (D-187, R01).** Main
`a35dfc3e21b57a0ab453e01898a0e42fa6d569ec` implements and registers the typed
`PricingReadV1`; the producer capability is present. This requirement remains unchecked for
Orders integration and read/evidence conformance. Orders owns a catalog port and SDK adapter;
no Pricing source/API change is selected here.

Methods take `&SecurityContext` and return `Result<_, CanonicalError>`:
`resolve(ResolveQuery) -> ResolvedBindings`, `price(PriceQuery) -> ImmutablePrice`, and
`current_revision(PlanQuery) -> RevisionRef`. Each query carries the seller `CatalogRef`.
These are not REST DTOs: resolve yields item/dimension cells, while current revision
yields only plan/revision IDs and revision number. The normative
[typed mapping and outstanding evidence](DESIGN.md#contract-03-pricing-read-mapping) retain
required revision availability/selection-coverage gaps. D-192 explicitly retires the specified legacy pin provenance from commercial storage; required business facts may not be fabricated or made optional by the adapter.

Pending conformance: published/current identity; scheduled/superseded rejection for new sales;
selected missing/uncovered/default cells; unselected uncovered cells; incomplete required inputs;
operation-specific missing-resource errors versus denied/unavailable/infrastructure failures;
seller/PDP isolation; provider-clock/UTC boundary behavior and catch-up writes. Existing
[provider tests](../../pricing/pricing/tests/pricing_read_sdk.rs) are source evidence, not Orders
integration sign-off. Pricing's `SafeRead` current-revision method can promote revisions and emit
audit/events; it does not create an acceptance receipt. Full Preview diagnostics, acceptance,
holds, pin persistence and activation remain separate reconciliation decisions. The service
permissions requirement below remains open.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pricing-catalog-tenant-reads`

**Revised scope; stable ID (D-194 supersedes D-160 identity construction).** Provision the
platform-authenticated Orders service principal for the seller, with explicit Pricing `plan:read`
and `price:read` and the Products SKU reads invoked by the provider. Orders obtains the authenticated
context through the supported S2S path; it never builds a tenant-scoped custom system actor.
Each query's `CatalogRef { tenant_id: seller_tenant_id }` narrows the target, not the authority.
No caller-tenant/latest-price fallback or Pricing-system impersonation is permitted. Historical
readability does not imply eligibility. The interim Products read has its own requirement below;
acceptance and downstream hold permissions are assigned separately by the
[D-194 call matrix](DESIGN.md#contract-08-commercial-service-authorization).

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-commercial-service-provisioning`

Platform AuthN/AuthZ, Pricing, Products and Subscriptions must agree and provision authenticated
seller-scoped Orders/Subscriptions service principals, least-privilege policies and rotation/revocation
handling under D-194. Orders needs acceptance create/read and related reads; Subscriptions needs
hold/read and related reads. Orders does not inherit hold or Products reference writes. Proposed
D-189 assessment, D-191 seller-policy discovery and D-193 terms resolver require agreed read grants
before delivery; Preview must not depend on acceptance create/hold. Keep original buyer/resource/
proposed-payer authorization independent, and preserve authenticated command principal identity
through D-188 retry/recovery. Credential refresh cannot silently change principal or replay identity.
Existing ordinary Products PDP routing is available; no privileged system branch extension is selected.
Real-provider positive and missing/revoked-grant tests, foreign-seller/payer denial, PDP outage,
Preview without command grants and credential/principal-change recovery are release prerequisites;
SDK availability alone does not close this requirement.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pricing-purchase-assessment`

**Reconciled scope; stable ID (D-189).** Pricing must implement the proposed separate
`PricingAssessmentV1::assess` SafeRead described in the [normative contract](DESIGN.md#contract-03-nonbinding-assessment).
It accepts basket intent without acceptance order/version/line identities and shares Pricing's
commercial evaluator with authoritative receipt admission. It returns all independent Pricing
predicate results, unavailable observations, proposed bindings and observation time. The
implemented `SellabilityV1::check` is an acceptance command and cannot satisfy this requirement.
This supersedes D-161's residual-only/withdrawn-assess proposal; existing safe reads retain their
D-187 adapter mapping. Required absent results remain unevaluable, never a successful full Preview.
Pricing SDK/provider work, seller-scoped least-privilege authorization, shared evaluation and
Orders composition/conformance are required. The D-189 no-commercial-artifact, parity, race,
incomplete-vector, TCV-withholding and retention scenarios are the completion evidence.
D-195 further requires the [versioned owner diagnostic contract](DESIGN.md#contract-03-diagnostic-mapping):
immutable rule/result profiles, operation-specific applicability, independently checkable roster
and expected coverage, stable predicate/reason/observation/selection identity and bounded typed
evidence. Orders owns the reviewed mapping registry and lossless aggregation, not Pricing rules.
Missing/unknown required results and invalid envelopes fail closed; genuine partial results remain
honest and do not claim completeness. Complete result vectors, multi-failure/unavailable-line,
TCV-only withholding, collision-free identities, redaction and profile compatibility tests are
required in addition to D-189. First-error acceptance commands need no synthetic full-vector API.
No upstream implementation or permission provisioning is claimed here.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pricing-acceptance-policy`

**New requirement (D-191).** Pricing owns the issued commercial deadline and must implement
[authorized seller-policy discovery/resolution](DESIGN.md#contract-03-commercial-deadline).
The proposed typed SafeRead returns seller/catalog identity, immutable positive policy version,
positive duration and observation time under PDP scope; acceptance must use the same seller
resolver. Current Pricing has one deployment policy (default version 1 / 86,400 seconds), no
policy-discovery SDK and no seller-specific resolver. Merely exposing deployment configuration
solves discovery, not the retained seller-specific requirement. Configuration/storage/provider
choice is upstream work; no Settings Service or uniform-policy downgrade is selected here.
Orders freezes the discovered version in each immutable D-188 attempt and copies issued
`hold_until` verbatim; no fallback version/duration. Policy races retain actual `UnsupportedTerms`
semantics without blindly retrying every such error. Required evidence includes multi-seller
isolation, absent/unavailable/denied/malformed policy distinctions, immutable version behavior,
stale-version refusal, unchanged historical receipts and nonbinding Preview. All implementation,
grants and conformance remain unchecked.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-initial-binding-acceptance`

**Revised scope; stable ID (D-190).** Implement the [accepted-price activation handoff](DESIGN.md#contract-03-accepted-price-activation).
The Pricing receipt/hold/live-eligibility capabilities already exist. Subscriptions, not Orders
or Preview, owns `hold` after choosing a durable activation instant, using only the receipt
selected by the authorized committed version. It checks exact identities/digests and performs
fresh `check_fulfilment` even after an exact hold replay. `FulfilmentQuery` has no `hold_until`
input. Initial activation honors the accepted price across successors; no pinned price-ID
comparison or `accepted-price-mismatch` admission branch remains.

Subscriptions must fence the committed order/version and current fulfillment attempt at activation
intent and enforce occupancy. The remote check and local commit are not one atomic operation:
R12's concrete fencing, cancellation and ambiguous-outcome recovery protocol remains a release
gate. Grants and cross-service tests are not delivered by the existing Pricing provider.
D-191 policy discovery/resolution and deadline conformance, plus D-192 frozen complete-receipt-to-first-period mapping implementation, remain
separate implementation prerequisites; never refresh accepted terms/descriptors implicitly.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-rating-evaluation`

Rating must publish a ClientHub SDK for one logical batched pre-purchase evaluation over the exact
selected bindings of an assessment. D-192 replaces D-167's legacy full `items[].chains[]` input: carry native selected bindings, explicit quantity/term/BillingTerms context and line/revision identity with `assessment_id` and `resolve_date`, plus seller/resource/payer context, contract and market. These are proposed nonbinding inputs before acceptance; issued receipt selection must match the evaluated inputs. The concrete DTO and unresolved purchase-model mappings remain required work, not an existing SDK. Unselected diagnostic cells cannot be billed, and this request is not the complete receiver period snapshot. Return
assessment identity, per-item/per-line and whole-order figures, gross/net/discount, promotion
status, currency/scale/rounding, three charge kinds and TCV.
Exact-price mode must not feed order pins to renewal resolve and walk to successors. One-time
preview does not synthesize a one-time recurring/usage rating unit. Minimum fees are Rating's.
Absent SDK, inconsistent binding identity or incomplete required figures is `evaluation-unavailable`.
Every submit/amendment requires totals; Preview retains only the explicitly allowed TCV withholding.
The D-192 amendment of D-167 governs the required evaluation DTO: Seam Atlas C06 (`acceptance_ref` plus digest,
`ExactAmount` outputs) is not consumed, figures are integer minor units stored verbatim, and
Rating's `fr-pre-purchase-evaluation` is aligned in documentation by D-197 to p1 exact-binding inputs and Rating-owned aggregation; SDK/provider delivery remains missing (Atlas ticket T3).

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-pricing-bundle-sellability`

**Narrowed scope; stable ID (D-161, read mapping amended by D-187).** Typed resolve supplies
item/dimension cells and covered bindings through `…-upreq-pricing-read-sdk`; it does not supply
the former treatment/included-allowance fields or an independent complete-roster attestation.
The [D-187 evidence mapping](DESIGN.md#contract-03-pricing-read-mapping) applies without relaxing
this obligation: resolve must return
the complete roster or an explicit unavailable answer, never a truncated matrix, and the
composition-only SKU sellability rule (a SKU sold only inside a plan) belongs to the residual
D-189 Pricing assessment of `…-upreq-pricing-purchase-assessment`. Deferred sold-as/grants and
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
D-197 amends Rating's caller-summation target; its implementation remains required. Define mixed recurring cycles and
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
themselves; Subscriptions reads selected receipt identities/digests through `get_version`), check `activation_deadline`
before each activation dispatch, treat a line as activated only at Subscriptions' `applied`, map
`oss_unconfirmed` and D-190's receiver refusals to its closed failure catalog
(`order-binding-expired` for expiry/explicit closure, `market-divergence` for market change; other receiver reasons require explicit mapping, never a blanket provider-failure conversion) and handle receiver refusal through compensation. Its approved-instance
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
contract alone. The former `gears/bss/fixtures`/Pricing `06-consumer-contracts.md` references
are historical and absent from the current checkout. S1-06 must deliver or select an actual
shared harness; no existing `CorpusEvaluator` or fixture runner is presumed. Orders states the
cases and each consumer owns its real-handler evaluator. The corpus is built with the first
consumer integration (Workflow), and remains unchecked until then. The platform supplies no consumer-side processed-event store: the Event Broker
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

**Capability present; integration remains open (D-200).** Main contains the
[Event Broker implementation](../../../system/event-broker/event-broker/src/lib.rs) and its
[ClientHub registration](../../../system/event-broker/event-broker/src/module.rs), as well as the
managed producer and toolkit outbox. The SDK-only statement in the platform inventory is stale.
Orders does not implement a broker or a private relay. This stable requirement now tracks actual
runtime wiring, schema/partition readiness, deployment and real-provider conformance, not delivery
of a missing implementation crate. Doubles permit local development but cannot satisfy this gate.
See the [selected integration contract](DESIGN.md#contract-01-event-platform-integration).

**S2-08 integration status (2026-10-06).** Orders binds the real `EventBrokerApi`, managed
Chained `DbProducer` and toolkit queue `bss-orders-events`; readiness requires every prerequisite
([evidence](implementation/EVENTS.md)). These platform findings remain open:

- **Partition-count read capability (requested).** No Event Broker API reports the partition
  count. `models::Topic` deliberately omits it because it is the broker's own configuration:
  per topic, `TopicSettingsEntry.partitions`, falling back to the instance-less topic-type
  entry and then `BUILT_IN_PARTITIONS` (8). `list_topic_segments` returns an empty manifest for
  any partition index, and producer cursors list only partitions already written, so neither
  can substitute, so startup cannot compare the counts. Event Broker maintainers are asked for
  a broker-level partition-count capability: an authorized read of a topic's resolved partition
  count (or the hint validation below), so that Orders startup can fail on a mismatch. **DESIGN
  §3.7 is amended by D-205** to match the platform: Orders requires an explicit, non-defaulted
  `events.broker_partitions` and never assumes the SDK default of 8; equality with the broker
  deployment is a deployment gate of this requirement (S6-05), set from one source and verified
  before release; startup **MUST** fail on a mismatch once the capability exists.
- **Silent loss under a partition-count mismatch (defect report for Event Broker / SDK
  maintainers; S2-08 gap review, 2026-10-06). Deferred escalation:** recorded at the user's
  request to be raised with the Event Broker owners later; nothing has been filed externally. Reproduced by the Orders PostgreSQL probe
  `a_wrong_declared_partition_count_silently_drops_events` against the real in-process broker:
  the producer declares 3 partitions and the broker is configured with 4. Twelve sequential
  committed events, each settled before the next, gave **8 stored, 2 dead-lettered and 2
  acknowledged without being stored**, with no queue retry or error. The result is deterministic.
  The S2-08 implementation report's figures (3 stored, 7 silently acknowledged) counted
  before the broker's asynchronous ingest outbox had persisted accepted events and are
  superseded. The control `a_matching_partition_count_stores_every_event` (4 declared, 4
  configured) stores all 12 events, and every publish is a first-time `Accepted`. The mechanism
  follows.
  1. *Producer side.* `producer/partitioning.rs` computes
     `partition_hint = murmur3(subject) % declared` and maps each hinted broker partition to one
     toolkit outbox partition. The chained processor (`producer/outbox.rs`
     `ProducerOutboxProcessor::cursor_for`/`refresh_cursor`) keys its cursor by that hint.
     `meta.sequence` is the outbox partition's `OutboxMessage.seq`.
  2. *Broker side.* Ingest ignores `meta.partition_hint`. It selects the partition from the
     configured count and checks the chain per `(producer, topic, selected partition)`
     (`event-broker/src/domain/ingest.rs`, `ProducerChainCheck`). Under a mismatch, one broker
     chain is therefore fed by several producer chains whose sequences collide.
  3. *Silent acknowledgement.* `ProducerChainCheck::decide`
     (`event-broker/src/domain/idempotency.rs`) returns `DuplicateIgnore` whenever
     `sequence == last`, without checking `previous` or event identity. A **different** event
     whose sequence equals the head of the selected chain is answered `Duplicate` and never
     stored. The SDK maps `Duplicate` to `MessageResult::Ok`. Observed: `seq=1, previous=-1`
     and `seq=4, previous=2`, both answered `Duplicate`.
  4. *Dead letters.* Other collisions get `SequenceViolation`. The SDK refreshes the cursor
     of the *hinted* partition, republishes, and on a second violation rejects with "producer
     outbox chained sequence divergence persisted after cursor refresh". Observed for `seq=7,
     previous=6` and `seq=3, previous=2`.
  5. *Latent second path.* `ProducerOutboxProcessor::handle_sequence_violation` acknowledges
     `seq <= previous` as already sequenced without publishing. Under a mismatch, `previous`
     is read from the wrong partition, so this path can also drop an event silently; this run
     did not exercise it.

  Without a mismatch, one broker chain is fed by exactly one outbox partition, in FIFO order.
  A sequence equal to the head is then a genuine retry: Orders observed it in the lost-ack
  tests, before and after a restart, with the same event ID and `meta.sequence`. The
  defect class is therefore gated on the count mismatch, which nothing currently detects.
  Requested fixes, owner Event Broker:
  - a broker-level partition-count read, or broker validation of `partition_hint` against
    the selected partition (refuse instead of re-routing);
  - `DuplicateIgnore` only when `previous` also links as for the original append, or the
    event ID matches;
  - an SDK guard that never acknowledges without a positive broker answer for the same
    event.

  Any producer republication protocol (`upreq-event-broker-dead-letter-recovery`) must also
  avoid the `seq <= previous` acknowledgement for a re-driven message.
- **PostgreSQL producer-registration defect (fixed in the owner).** The SDK's managed
  registration store bound `created_at`/`updated_at` as RFC 3339 text, which PostgreSQL refuses
  for its `TIMESTAMPTZ` columns, so no managed producer could register on PostgreSQL. SDK tests
  ran only SQLite. The workspace carries exactly the upstream fix on branch
  `fix/eb-sdk-producer-registration-timestamptz` (commit `797f902cf`): the columns are mapped
  as `DateTimeUtc`, `Utc::now()` is bound, and `registration_tests.rs` adds SQLite and opt-in
  PostgreSQL round trips plus legacy-text reload. It drops out of the Orders change set once that
  branch merges. Orders' PostgreSQL integration suite is a further regression.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-event-broker-cursor-retry`

**Transient producer cursor-recovery failures.** The Event Broker SDK's managed Chained
producer **MUST** classify transport and rate-limit failures during initial cursor recovery as
`MessageResult::Retry`. These failures **MUST NOT** dead-letter the queued event or advance the
toolkit queue-partition cursor. Permanent failures retain the SDK's rejection policy; retry
cadence remains owned by toolkit-db.

**Owner:** Event Broker SDK maintainers.

**Source capability present (D-200).** The SDK's
[`producer/outbox.rs`](../../../system/event-broker/event-broker-sdk/src/producer/outbox.rs)
now maps initial cursor-fetch transport and rate-limit errors to `Retry`; the previously recorded
all-errors-to-`Reject` defect is fixed. Source history identifies commit `c7de7b80ae` (2026-09-23).
Existing SDK tests cover startup cursor recovery, sequence refresh and publish rate-limit retry;
this inspection did not establish the dedicated empty-cache transient cursor-fetch regression.
**S2-08 (2026-10-06):** the dedicated regression now exists in the owner's suite,
`outbox_processor_retries_transient_initial_cursor_read_with_empty_cache`
(`event-broker-sdk/tests/producer/outbox.rs`, real in-process broker). Orders' PostgreSQL suite
repeats it through the running toolkit workers. Deployed-revision evidence remains open.

**Release gate:** record the deployed revision containing this behavior and passing the complete
regression below. No new fix PR is required merely to satisfy the superseded defect description;
a successful health check or presence of the source branch is not regression/deployment evidence.
**Tracking status:** open for deployment and conformance; code behavior is present.

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
The implemented LocalBroker preserves the explicit event tenant and ingest checks authorization
against that tenant. D-200 therefore tracks root identity/grants and real-provider verification,
not a missing tenant override API; any conflicting publisher-context documentation remains a
shared documentation follow-up. Root-scoped events **MUST NOT** become
readable merely because a principal has customer, partner or seller access to an Orders API.

**Owners:** Event Broker, platform tenant identity and authorization maintainers, with the
Workflow, Subscriptions and Billing owners confirming their consumer grants.

**Release gate and tracking:** open. Orders production deployment requires a documented root UUID
source, the concrete producer/consumer identities and grants, and passing broker integration
evidence. No verified identity source or deployed grant configuration has been recorded.
**S2-08 (2026-10-06):** Orders resolves the root from the platform
`TenantResolverClient::get_root_tenant` before readiness. It requires an active parentless tenant
and stamps that UUID on every envelope. The tenant-identity owners still have to confirm that
call as the authoritative source. Locally, the broker authorized only the root envelope tenant,
and a missing root grant dead-lettered without changing the order. The bundled rules AuthZ
provider cannot express the broker's property-less `event_type` produce check: any rule
constraint fails closed. Deployed grants therefore need a PDP able to grant that decision.

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

**Administrative free-text minimization (D-204).** `display_label`/`internal_notes` before/after
values are a keyed `hmac-sha256:v1:<key_id>:<hex>` under required Orders configuration, replacing
the S2-06 unkeyed digest. Privacy/Legal still owns whether a keyed pseudonym of that content may
be retained and for how long; the minimizer is one replaceable function if that answer changes it.

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

**Orders-side carrier (D-202, decided 2026-10-06).** The Orders REST boundary accepts the proof
reference in the optional `X-Delegation-Proof-Ref` header (opaque, 1–512 printable ASCII,
credential-free; malformed or repeated is `request-invalid` before authorization; never logged,
echoed or forwarded except as PolicyEnforcer request context). It fills the same
`CallMeta.delegation_proof_ref` as the SDK ([08 §4.4](DESIGN.md#contract-08-4-4)). This fixes the
Orders boundary only. Items 1–3 above still ask the platform for its own carrier and deny codes;
until then the reference travels as the reserved, never-compiled resource property
`delegation_proof_ref`, and the deny codes `delegation_proof_required`/`delegation_proof_invalid`
are Orders constants pending platform agreement.

**Integration and E2E provider (2026-10-06).** The fail-closed `rules-authz-plugin` is a real
resolver provider for integration and live E2E, not the production provider. It also grants
Event Broker's property-less `event_type` `produce` check to the Orders producer principal through
an exact unconditional grant, with no wildcard or allow-all, and its tenant-scope `produce` check
through an ordinary rule constrained to the platform root tenant. Production provisioning of
those producer grants remains part of this requirement and of `…-upreq-event-broker-runtime`.

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

Products must admit the authenticated Orders service principal (D-194) to `ProductsClient::get_sku` in the seller
tenant (a SKU read grant), so the gate can read `Sku.sellable` and `Sku.lifecycle` for each consumed
SKU (DESIGN §4.1, D-160). `get_sku` is a scoped read: the tenant argument narrows, it never grants.
P-D-222 refuses the Pricing system actor at every REST door; D-194 uses the ordinary service
PDP path instead of impersonating it. The resolve-echo alternative is withdrawn (D-171). The read is
interim: it retires when the proposed D-189 nonbinding assessment supplies authoritative SKU results
(amending D-177; the existing acceptance command alone is insufficient), after which Seam Atlas P9
holds. Until the grant exists the row is `catalog-predicate-unevaluable`.

- [ ] `p1` - **ID**: `cpt-cf-bss-orders-lifecycle-upreq-sku-protection`

**Pricing coordinates release; stable ID (D-164, amended by D-196).** SKU protection is inherited
from revision `plan_item` references. Pricing is the sole Products reference owner; Orders asks
for no registry owner, kind, receipt or reserve/confirm/release grant. Current Pricing retains
published/superseded revision references: revision retirement/release is deferred by Pricing
D-410, and book archival releases entry references only. This is existing conservative protection,
not an implemented usage-aware cleanup feature.

Before enabling revision-reference release, deliver the [D-196 closure/drain and usage contract](DESIGN.md#contract-03-revision-reference-protection).
Pricing coordinates authorized Orders and Subscriptions reports itself. It MUST durably close new
admission, fence and drain older work, and prove absence of every remaining holder under the same
release generation before releasing references. A zero count from an ordinary read is insufficient.
Reports cover D-188 preparations that can still commit, committed nonterminal versions, and the
handoff to durable Subscriptions ownership without a protection gap. Missing, stale, denied or
unavailable reports retain references. Receipts from failed attempts confer no activation rights.
The SDK/report shapes, closure acknowledgements, grants, recovery and cross-owner tests remain
unimplemented release prerequisites; no existing API is asserted. R12 activation/cardinality
fencing remains a separate dependency. If a SKU becomes retired, D-190 fresh eligibility refusal
and failure/compensation handling still apply; this decision adds no force-retire capability.

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

**Status (2026-10-07, S2-12, D-211):** the fallback is implemented and verified
(`api::rest::throttle::PerOrderLimiter`, 20/min baseline, bounded key store, consulted before
boundary validation; real-PostgreSQL and live E2E evidence in
[MILESTONE](implementation/MILESTONE.md)). The request to the gateway stands: the gear-local
limiter is per replica and Orders removes it when the composite key lands.

## 3. Priorities

| Priority | Requirements |
|----------|-------------|
| `p1` (critical) | `…-upreq-subscription-start-instant`, `…-upreq-pre-subscription-billing-terms`, `…-upreq-overlap-presence-read`, `…-upreq-compensation-cancel-reason`, `…-upreq-pre-subscription-evaluation`, `…-upreq-tcv-with-annualisation`, `…-upreq-external-reference-propagation`, `…-upreq-delegation-proof-credential`, `…-upreq-authorization-outcome`, `…-upreq-event-consumer-conformance`, `…-upreq-workflow-amendment-verdict`, `…-upreq-workflow-overdue-escalation`, `…-upreq-event-broker-runtime`, `…-upreq-event-broker-cursor-retry`, `…-upreq-event-broker-dead-letter-recovery`, `…-upreq-event-broker-root-tenancy`, `…-upreq-event-delivery-observability`, `…-upreq-catalog-subscription-product-key`, `…-upreq-pricing-read-sdk`, `…-upreq-pricing-catalog-tenant-reads`, `…-upreq-commercial-service-provisioning`, `…-upreq-products-sku-read-grant`, `…-upreq-pricing-purchase-assessment`, `…-upreq-pricing-acceptance-policy`, `…-upreq-initial-binding-acceptance`, `…-upreq-sku-protection`, `…-upreq-rating-evaluation`, `…-upreq-payer-commercial-profile`, `…-upreq-contract-party-eligibility`, `…-upreq-contract-acceptance-declaration`, `…-upreq-settle-create`, `…-upreq-intent-status-read`, `…-upreq-transition-outcome-echo`, `…-upreq-overlap-activation-atomicity` (release gate for submit/activation, D-180) |
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

The p1 requirements `…-upreq-pricing-read-sdk`, `…-upreq-pricing-purchase-assessment` (D-189 nonbinding shared assessment), `…-upreq-pricing-acceptance-policy` (D-191 seller-policy discovery/resolution), `…-upreq-initial-binding-acceptance` (D-190 committed-receipt activation),
`…-upreq-sku-protection` (narrowed to the release trigger) and `…-upreq-workflow-pricebook-contracts`
are additional release prerequisites. Existing seller-read, Rating, composition and overlap-key IDs
carry the revised contracts above. The 2026-09-30 revision (D-159–D-168) pulls each ask back onto the
seam that already exists where one does; no ID marked unchecked is represented as delivered.

**Atlas overlay alignment (2026-10-02).** Three asks were added to §2.1 (`…-upreq-settle-create`,
`…-upreq-intent-status-read`, `…-upreq-transition-outcome-echo`), co-signed with the Workflow
branch's `SUB-O13`/`SUB-O16`; §2.2, §2.4, §2.5, §2.6, §2.10 and §2.11 carry field shapes and the
fork-tip facts (Pricing D-454/D-460/D-467/D-469, Products P-D-222, Workflow D-193–D-199).
Decisions D-169–D-178 record the Orders-side positions; Q-32 (the hold) is now resolved by D-190; Q-33 (add-ons and change orders) remains open.


### R02 reconciliation: existing durable acceptance contract (D-188)

`SellabilityV1::check` already commits immutable receipts uniquely by seller/order/version/line. Orders adopts permanently reserved candidate version numbers through the [D-188 protocol](DESIGN.md#contract-01-commercial-attempt); no Pricing SDK or uniqueness change is requested. Orders still owns its adapter, durable attempts, authorization/grants, immutable input mapping and recovery. Cross-service conformance must prove exact-key replay after a lost response, partial multi-line failure followed by edited resubmission on a fresh candidate, and rejection of reserved/unselected receipts by downstream activation. The receipt command is not the previously assumed residual predicate verdict or a Preview read: those evidence/API gaps remain open. R03 is separately resolved by D-189; R04 follows D-190; D-191 resolves policy/deadline authority, with seller-policy discovery/resolution and deadline conformance still missing.


### R06 downstream adoption requirement (D-192; not delivered)

The requirement is registered in [§2.1](#21-subscriptions) as
`cpt-cf-bss-orders-lifecycle-upreq-frozen-commercial-materialization`, where requirement IDs are
defined; its full text is kept there unchanged.

### R11 exact-binding evaluation delivery (D-197; missing runtime)

Implement the [D-197 request/result and failure contract](DESIGN.md#contract-03-rating-purchase-evaluation), including authorized service grants, model/profile declaration, canonical input identity, complete minor-unit aggregates, single TCV carrier and finite/rolling/mixed basis. Orders freezes evaluation in D-188 and validates receipt equality. No arithmetic adapter or test double closes this prerequisite. Joint tests cover temporal/rounding/model boundaries, usage versus zero, missing scope, malformed responses, Preview-only withholding, changed catalog and replay. The Rating PRD/SEAMS amendments select the target; they are not deployed-provider evidence or owner signoff. Indicative tax remains separately open.


**D-198 delivery prerequisite (unchecked):** Subscriptions/Workflow must implement the [receiver admission, complete inventory barriers, cancellation/hold linearization and ambiguous-outcome recovery protocol](DESIGN.md#contract-06-activation-admission). Include pending admissions in capacity, protect policy-generation updates, retain unknown claims, and reject unselected/reserved version receipts. Existing hold/eligibility reads cannot supply this authority. Engine coordination SDKs, receiver typed evidence, grants and cross-service tests remain missing; target scope selection does not establish peer sign-off.

### R13 commercial owner readiness (D-199; not delivered)

The [owner contract/readiness matrix](DESIGN.md#contract-05-commercial-owner-readiness) governs §2.4–2.6 and §2.11 delivery: record SDK/schema, actual provider, principal/grants, scope, evidence identity, freshness and real conformance for each affected path. Account Management's existing tenant/metadata APIs do not yet define the commercial profile; Ledger payments and admission-control are not Payments authorization; BSS Approvals does not establish order approval policy.

- [ ] Deliver authoritative payer profile and PDP-verified delegation, including absent/invalid/foreign-axis tests.
- [ ] Deliver batched Contracts status/eligibility/declaration and live guard-time reads with no unavailable fallback.
- [ ] Deliver Payments authorize/read-by-request and Workflow restart recovery; bind request to version/payer/amount provenance separately from TCV.
- [ ] Deliver current-version approval/consent independence, amendment reapproval and Workflow production integration with actual owner providers.

Lifecycle continues consuming Workflow adapter facts under D-175; no new direct provider verification API is introduced. Runtime, permissions and owner signoff remain open, and doubles never close production requirements.
