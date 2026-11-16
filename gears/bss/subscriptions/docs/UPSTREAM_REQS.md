---
description: "External requirements on Subscriptions and the provider contracts needed to deliver them."
---

# UPSTREAM_REQS: Subscriptions

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Requesting gears](#12-requesting-gears)
- [2. Requirements](#2-requirements)
  - [2.1 Orders operations](#21-orders-operations)
  - [2.2 Pricing and commercial identity](#22-pricing-and-commercial-identity)
  - [2.3 Rating and Billing](#23-rating-and-billing)
  - [2.4 Commercial posture and retention](#24-commercial-posture-and-retention)
  - [2.5 Provider prerequisites and bilateral acceptance](#25-provider-prerequisites-and-bilateral-acceptance)
- [3. Priorities](#3-priorities)
- [4. Traceability](#4-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

This register defines what other gears require from Subscriptions and which provider contracts those obligations depend on. The [PRD](PRD.md) defines scope. Features and consumer contracts define the protocols, failure handling and acceptance cases.

### 1.2 Requesting gears

| Consumer | Needs from Subscriptions |
| --- | --- |
| Orders Lifecycle, Workflow and Changes | Recoverable operations, order identity, occupancy, atomic composition changes and the termination receipt that ends an Orders grant (SUB-D-124) |
| Accounts | Outcomes of the uses that Subscriptions authorizes (renewal, quantity increase, transfer), each with its `use_version`, and the `assignment_status` read for current views (SUB-D-124) |
| Rating | Accepted terms, period facts, historical attribution, scope proofs and recovery |
| Billing | Commercial group membership, exact seals, source lineage and service boundaries |
| Entitlements | Commercial posture, preparation context, disposition and authoritative activation resolution |

## 2. Requirements

### 2.1 Orders operations

#### Durable operation protocol

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-order-intents`

Subscriptions MUST expose separate draft creation and activation, bind operations to the acquisition origin identity (`AcquisitionOriginV1`, SUB-D-71; for Orders: order, version, attempt, generation, line), and distinguish acceptance from application. Orders is the origin provider: it owns accepted purchase evidence, reservation at confirmation, capture ordering, compensation, hold/unpause/revoke and remediation of a closed attempt. Orders MUST treat `preparing` as a non-served, non-terminal status that counts as a draft for occupancy (SUB-D-58); the Orders contract D-242 needs this alignment. Replay, withdrawal, status lookup and uncertain-create settlement MUST preserve one acquisition and its financial obligations. The create business key excludes generation; its result or no-create tombstone covers all generations of the same attempt. The reservation key excludes both attempt and generation, is kept per ordering tenant and has a separate release tombstone; Orders Lifecycle reserves and releases through the reservation commands, in the one ordering tenant that orders the line, and a reserve in another tenant is outside the contract (SUB-D-100, SUB-D-101). Attempt settlement MUST NOT release that reservation. Orders Workflow MUST read an activation's closure from the activation progress (`phase = closed`, with its reason) and its cleanup from `pendingCleanup`, not from a terminal operation state: a closed activation stays `approved` until its last preparation effect resolves, whatever its closure reason (SUB-D-81). Exact lookup MUST expose the stable create outcome separately from receiver generation state, including recovery after a changed envelope conflicts with a cached transport request. A successor attempt requires fenced predecessor settlement. The create key and the receiver ledger carry no ordering tenant, so one origin line binds at most one subscription in the platform (SUB-D-94), and a line holds one fence whatever its attempt: until S-U01 opens, Orders cannot settle a second abandoned attempt of one line (SUB-D-95). Orders MUST send a hold as a `pause` command and its release as `unpause` per receiver generation, and a revocation as `revoke`; Subscriptions keeps the receiver ledger `(order, version, attempt, generation, line)` with the states open, paused, revoked and settled, and a paused generation can neither admit new preparation nor commit active (SUB-D-59). Every receiver command, reservation command and exact lookup names the ordering tenant explicitly; the Orders service identity acts in a customer's ordering tenant because its grant reaches that tenant from above (SUB-D-83). Same-generation `unpause` MUST continue the existing preparation with its retained readiness and remaining deadline. This resume protocol is a pending owner request: current Orders instead issues a successor generation and leaves the old grant fenced. A successor grant alone MUST NOT take over paused preparation, reset its deadline or authorize cleanup. S-U01 remains closed until Orders adopts the requested protocol or both owners replace it with an explicit preparation-transfer contract and executed joint cases. Subscriptions runs the Orders D-190 fresh `check_fulfilment` before activation using the [execution-check result mapping](DESIGN.md#execution-check-results) for commercial refusals and distinct authorization, evidence and dependency failures (SUB-D-60); it never reprices the accepted purchase. Held entitlement preparation and reservation-backed payment capture MAY precede T; neither establishes `serviceActivatedAt`, service-period accrual or a billable service fact. The requested Orders wording must distinguish served service from this preparation. The current Orders sentence is not yet aligned; adopting this distinction and `preparing` is an S-U01 prerequisite.

Initial free-period and credit trial admission also requires the S-U01 AP08 reservation at confirmation. The owner mapping must prevent two concurrent orders from consuming the same trial eligibility. AP-C16 and the initial-trial admission race in [feature 06](features/06-trials.md) are joint evidence for both variants; the absence of a promotional grant in a free-period trial does not remove this gate.

- **Consumer:** Orders Lifecycle and Workflow.
- **Contract:** [Activation](DESIGN.md#332-pricing-acceptance-and-accepted-time), [foundation](features/01-foundation.md).
- **Acceptance:** V01–V02 and AP-C01–AP-C16 cover lost replies, cancellation, partial order fulfillment, recovery and pause/resume races, including pause before preparation admission, preservation of an existing preparation and its remaining deadline, and the complete execution-check result mapping.

#### Occupancy and atomic entry

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-occupancy`

Subscriptions MUST expose the authoritative scope key, limit and policy version with separate active, retained-suspended and order-reserved counts. Admission and claim conversion MUST be atomic. Confirmation reserves before capture; activation or unchanged-key resume consumes its own right once. Paid or uncertain claims survive technical expiry. Lowering the limit does not evict protected claims. The read returns the claim set, one claim per distinct key of the resulting composition; a component addition claims new keys without releasing existing ones. For each key, compare active plus retained-suspended plus order-reserved rights with the effective limit, counting each right once. Another holder alone does not mean the key is full. A new claim that would exceed capacity refuses atomically and is visible to the Orders gate before approval (SUB-D-63). With limit 3 and one existing right, a second right can be admitted; with three existing rights, it cannot.

- **Consumer:** Orders Lifecycle and Changes.
- **Contract:** [Foundation](features/01-foundation.md), [activation](DESIGN.md#332-pricing-acceptance-and-accepted-time).
- **Acceptance:** V03 covers concurrent admission, suspension, reservation conversion, failed key movement and fenced release.

#### Atomic composition change

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-composite-change`

Subscriptions MUST apply an admitted multi-item change to one subscription in one transaction against the expected revision. Validate the resulting bounds, eligibility and occupancy together. Increases and additions require an active subscription, or a draft subscription once the S-U01 draft-change profile is enabled; suspended purchases refuse at submission and commit. Unsupported dates or items refuse the whole request before effects. A change order that passes the Orders submit gate (which admits any non-terminal target) and reaches the Subscriptions active-target, suspended-purchase or closed draft-change guard receives a typed refusal that names the guard; Orders MUST hold the change order for operator resolution (a separate guarded resume, or the S-U01 draft profile once enabled) instead of treating approval as admission. Orders MUST reject a requested effective date on a change line before approval in this version (CHG-S5 refused); an absent date means application at the Subscriptions commit, never the day of submission. This is the required owner behavior, not a claim that the current Orders contract already enforces it. A change that still arrives with a requested date receives the typed future-boundary refusal. The composition read exposes each line's accepted binding reference, pending or failed cancellation intent, confirmed charging cutoff and aggregate revision. Before approval, Orders MUST refuse an increase whose delta binding differs from the existing quantity's binding while S-U08 is closed. Orders MUST also refuse purchases with a pending or failed cancellation intent or a confirmed charging cutoff. Subscriptions rechecks these guards at commit. A refusal after approval keeps the change order held for resolution: withdraw or resolve the cancellation only where its own contract permits, or cancel the change order. A change order cannot bypass a confirmed cutoff. The S-U16 Orders contract MUST also check the pending-conversion purchase restriction before approval and refuse while it applies. The composition read supplies that restriction with its revision; Subscriptions rechecks the trial episode at commit. This mapping and TP-C30 remain required before enabling changes to trial subscriptions.

The current Orders Changes contract already holds failed applications for retry or cancellation. It still needs the specific pre-approval checks and typed mappings above, including cancellation and different-binding refusal. S-U16 stays closed until bilateral cases prove both early refusal and the commit-time race handling.

- **Consumer:** Orders Changes.
- **Contract:** [Composition changes](features/03-changes.md).
- **Acceptance:** V06 covers suspension and cancellation races, different-binding pre-approval refusal, stale revisions, invalid bounds and atomic refusal.

#### Requested acquisition-owner steps

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-acquisition-owner-steps`

Protected acquisition depends on steps that only Orders Lifecycle, Workflow and Payments can perform. Subscriptions cannot build them; [acquisition recovery](features/01-foundation.md#acquisition-recovery) keeps the Subscriptions side of each: its fences, reservations and recovery. The steps below are requests until their owners accept them in their own registers; the affected profiles stay gated under S-U01 and S-U07 until then (SUB-D-92).

- **Capture-start decision (Orders Lifecycle and Workflow).** Orders Lifecycle atomically records cancellation winning before capture-start, or capture-start authority plus outbox, under the same acquisition/version guard. No remote call occurs inside that transaction. Workflow delivers/reconciles that decision. A crash before commit creates no dispatch authority; a lost reply after commit recovers the same decision.
- **Capture identity (Orders Lifecycle and Payments).** Orders Lifecycle and Payments bind the exact acquisition and version, line set, financial obligation, claim and commitment versions, scope and digest to one capture identity.
- **Capture against the reservation (Billing/Payments).** Billing/Payments captures only against the agreed durable reservation and commercial evidence for the captured scope and its capture and cancellation fence.
- **Capture deduplication (Payments).** Payments deduplicates execution by the capture business identity, independently of transport keys. Changed content conflicts. Durable execution and provider recovery must prevent a second debit after response loss; an expiring HTTP cache is insufficient. Unknown capture remains pending under the same identity.
- **Cancellation against capture start (Orders, Payments and Billing).** Cancellation winning before capture-start fences delayed dispatch against debit. If capture-start won, cancellation remains available through the applicable direct or workflow-mediated route. Suppress outstanding execution where supported; only durable non-execution proof establishes no debit. Otherwise recover capture, including late success, and record Billing's required remedy. Capture-start is neither proof of bank execution nor a promise that cancellation will prevent debit.
- **Release of a held place (Orders Lifecycle).** Orders Lifecycle always releases a line's held place that no activation will consume, in the ordering tenant that holds it. Nothing else frees it: there is no timeout and no sweeper. An incident in which Orders does not release is resolved by the same `release`, granted to an operator by policy; the capture fence still guards it (SUB-D-101).
- **Partial orders (Workflow).** Workflow creates drafts, retains the common requested-date barrier and obtains/replays the committed spawn signal before dispatch. An ambiguous signal acknowledgement permits recovery only. That signal closes the documented direct-cancel boundary; it proves neither payment nor activation and does not suppress later mediated hold/cancel guards.

- **Consumer:** Orders Lifecycle, Workflow and Payments.
- **Contract:** [Acquisition recovery](features/01-foundation.md#acquisition-recovery), [capture arbitration](DESIGN.md#332-pricing-acceptance-and-accepted-time).
- **Acceptance:** AP-C07, AP-C08 and the capture recovery paths of feature 01; owner acceptance under S-U01 and S-U07.

#### Historical Orders identifiers

Orders documents cite identifiers from the removed Subscriptions seam map and from the Orders Changes PRD. This table maps them to the registers above; Orders keeps its own [semantic alias map](../../orders/docs/UPSTREAM_REQS.md#subscription-seam-alias-map).

| Historical identifier | Meaning | Register here |
| --- | --- | --- |
| SUB-O1, SUB-O2 | Compensation cancellation reason; order reference and create key | `cpt-cf-bss-subscriptions-upreq-order-intents` |
| SUB-O3, SUB-O10 | Externally callable draft/activate pair; actual service start T | `cpt-cf-bss-subscriptions-upreq-order-intents` |
| SUB-O4 | Nonbinding pre-purchase sellability check | Pricing-owned; not a Subscriptions register |
| SUB-O5, SUB-G1 | Overlap-occupancy read and the scope key (formerly `catalogSubscriptionProductKey`, now the versioned-policy `overlapScopeKey`) | `cpt-cf-bss-subscriptions-upreq-occupancy` |
| SUB-O6 | Atomic multi-subscription orchestration | No bulk API; one operation per line, Workflow orchestrates |
| CHG-S1…CHG-S4 | Order reference on changes, transactional multi-item change, overlap exemption for the target, composition read with revision | `cpt-cf-bss-subscriptions-upreq-composite-change` |
| CHG-S5 | Requested effective date on a change | Refused in this version: a requested date refuses before effects ([PRD §6.3](PRD.md#63-plan-changes-upgrade--downgrade)); the initial Orders Changes profile must reject any requested date before approval, and an absent date means application at the Subscriptions commit. Scheduled changes stay deferred. |

The historical Contracts and Pricing references retain their original meaning:

| Historical identifier | Current requirement or disposition |
| --- | --- |
| SUB-C1 | `cpt-cf-bss-subscriptions-contract-contracts-input` and `cpt-cf-bss-subscriptions-fr-platform-defaults`, gated by S-U06. The historical notice ladder is deferred under SUB-D-69; supported terms require no notice or separate assent. |
| SUB-C2 | `cpt-cf-bss-subscriptions-fr-ramp-execution`, deferred under SUB-D-04. No current ramp execution requirement. |
| SUB-C3 | Contracts defines the prepaid-pool agreement; Rating owns its true-up valuation. No current Subscriptions pool execution or drawdown requirement. |
| SUB-C4 | `cpt-cf-bss-subscriptions-fr-activation-instants` and SUB-D-05. Separate customer acceptance is deferred. |
| SUB-C5 | `cpt-cf-bss-subscriptions-upreq-accepted-bindings` and `cpt-cf-bss-subscriptions-fr-accepted-price-bindings` preserve accepted Contract and commercial provenance. The historical step-5 overlay is not the current valuation protocol. |
| SUB-P8 | Historical Pricing per-price-ID non-terminal subscription presence read for retirement/window changes. It is not active for the delivered PriceBook provider. The separate occupancy read is `cpt-cf-bss-subscriptions-upreq-occupancy`. |

### 2.2 Pricing and commercial identity

#### Accepted commercial inputs

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-accepted-bindings`

Subscriptions MUST retain immutable accepted terms and purchase-lot provenance. Activation executes the accepted purchase without repricing or live catalog re-admission, but MUST obtain fresh execution eligibility under SUB-D-60. Renewal and mid-term continuation require accepted authority and complete effective-dated slices. Missing continuation evidence produces an explicit unresolved fact version; it never selects current prices or silently values the gap at zero.

Pricing MUST provide a versioned AP03-aware execution check before the protected future-purchase profile is enabled. It MUST accept the durable commitment reference/version and bind its result to the acquisition/tenant scope, accepted terms digest and execution time. It MUST verify the commitment's current disposition and explicit execution restrictions and return freshness evidence suitable for the guarded commit. Ordinary quote expiry or new-sale-only changes MUST NOT revoke valid protected terms; actual execution restrictions, market change and mismatched identity/terms MUST retain distinct reasons and agreed Orders mappings. The result contract MUST distinguish confirmed closure/temporary end from other causes of V1 `PriceClosed`, or explicitly agree on a mapping for that broader result; the code alone is not proof of expiry. Current V1 `FulfilmentQuery` has no commitment input, and its hold/window rules remain unchanged. Subscriptions MUST NOT locally ignore a V1 refusal. Missing support or an unmapped provider result keeps S-U01 closed before protected acceptance/capture.

**One Subscriptions service principal.** The platform MUST provide one service principal for the Subscriptions gear (SUB-D-126): a confidential client whose service account is a service subject (`gts.cf.core.security.subject_service.v1~`) homed in the platform-root tenant, with narrow action and resource roles assigned at the platform root. Subscriptions obtains its context only by the client-credentials exchange of the AuthN Resolver, never by building one. Its roles cover the work that the gear does as itself: Pricing's acceptance read, hold and fresh execution check of activation (SUB-D-60, SUB-D-108); the discovery and the reads and writes in each tenant of the activation driver, the posture worker and the period reconciler (SUB-D-123); the production of the fact events (SUB-D-121); and the Accounts consumer actions (SUB-D-124). A role at the root is not a scope: the decision point issues a single-target scope for every call (SUB-D-125). Until the principal is provided, every create refuses `capability_closed` (`accepted_terms`) and stores nothing; the workers use the gear's discovery warrant with per-row tenant scopes, and a drive asks its providers as a first-party identity of the gear; and the event producer's context is built in the process. Each of these is a known gap. A principal that is not a service subject of the platform-root tenant, or that cannot be obtained, makes a create `dependency_unavailable`. Owner: platform AuthN and account provisioning.

**Pricing service read path.** Pricing MUST expose to a service principal a pinned read of an acceptance receipt, and the hold and the fresh execution check of activation, in the catalog of an explicit tenant, with that catalog tenant as the context tenant of the decision and under its single-target scope, with registered service permissions ([commercial model §3.4](../../../../docs/arch/bss-commercial-model/DESIGN.md#34-internal-dependencies) and the Products and Pricing row of its §4.1). Pricing's acceptance read evaluates the decision at the subject's own tenant and names no context tenant (Pricing D-523), so it refuses the gear's principal, which is homed at the platform root. Subscriptions names the seller tenant from the create and checks it against the receipt (SUB-D-108). Until Pricing exposes this path, every create refuses `capability_closed` (`accepted_terms`) and stores nothing; Pricing refusing the gear's principal makes a create `dependency_unavailable`. Owner: Pricing.

**Outbox attempt count.** The ToolKit transactional outbox MUST NOT let its attempt count stop a partition. The relay of the fact events runs on that outbox (SUB-D-121). Today the outbox counts the attempts on a partition in a 16-bit number that only progress resets, so a failure that persists for about 32,000 attempts stops that partition for good: every later lease of it fails, and its events wait until the count is reset by hand. The outbox is asked to saturate the count, or to keep a count that never stops the partition. Until then, a relay failure other than a refused event that persists while the producer is bound can stop the partition it falls on. Owner: platform ToolKit outbox.

**Open point: receipt size.** A create freezes the complete acceptance receipt as evidence, bounded at 8 MiB of canonical JSON. The bound is derived from the caps that Pricing and Products publish, with headroom (SUB-D-108). Pricing caps neither the bands of a tier ladder nor the length of a market region, so no finite bound admits every receipt Pricing could issue. A receipt above the bound is a corrupt answer: its create answers `dependency_unavailable` and stores nothing. Pricing is asked to cap both, or to agree a receipt size bound with Subscriptions. Pricing's caps also judge only new writes (Pricing D-457): a row stored before them stays readable however long its texts are, and a text that names a stored row, such as a price's dimension value, is never capped. A receipt can therefore carry stored texts longer than the caps the bound is derived from, and a cell can name a dimension value longer than the 255 characters that a composition admits (SUB-D-107). Such a receipt is refused on every attempt and nothing is stored: a create that names such a dimension value fails its own bound (400), and a receipt whose texts break the create's bounds, or whose document is over the bound, is a corrupt answer (`dependency_unavailable`). Pricing is also asked whether such stored rows exist, and how long their texts are.

- **Consumers:** Orders, Rating and Billing.
- **Contract:** [Activation](DESIGN.md#332-pricing-acceptance-and-accepted-time), [composition](features/02-composition.md), [Rating](DESIGN.md#rating-result-contract).
- **Acceptance:** V04–V05 and V08 cover future purchases, catalog changes, continuation and historical replay.

### 2.3 Rating and Billing

#### Versioned facts and commercial membership

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-versioned-facts`

Subscriptions MUST publish full immutable versions of money-free facts and seal commercial membership with exact fact ID, version and digest. Corrections preserve occurrence and purchase lineage. Consumers match results to the exact source version; commercial completeness, rated completeness and financial execution remain separate.

- **Consumers:** Rating and Billing.
- **Contract:** [Commercial facts](features/08-commercial-facts.md), [Rating/Billing](DESIGN.md#financial-consumer-boundary).
- **Acceptance:** V07–V08, V10 and financial consumer cases cover replacement, reordering and correction without duplicate obligations.

#### Usage period authority

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-usage-period`

Subscriptions MUST publish a parent usage fact when its commercial period opens, including a partial first period. Billing terms and usage policy are independent inputs. Rating derives and schedules child windows.

- **Consumer:** Rating.
- **Contract:** [Commercial facts](features/08-commercial-facts.md), [Rating](DESIGN.md#rating-result-contract).
- **Acceptance:** V07 covers one monthly parent, complete hourly coverage and recovery of a missing opening event.

#### Authoritative attribution stream

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-attribution`

Subscriptions MUST publish versioned historical resource/subject-to-component attribution with resource-tenant checkpoints. Gaps and ambiguity are explicit. Current composition cannot replace historical ownership or payer evidence.

- **Consumer:** Rating.
- **Contract:** [Attribution and recovery](features/09-consumer-contracts.md).
- **Acceptance:** V09 covers late evidence, deleted resources, duplicate segments and missing history.

#### Sealed ownership scope

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-scope-proof`

Subscriptions MUST return a versioned sealed scope or explicit pending result for batched ownership queries. Bind each proof to the fact version, rating window and inventory barrier. Ownership completeness does not establish usage completeness.

- **Consumer:** Rating.
- **Contract:** [Scope proofs](features/09-consumer-contracts.md).
- **Acceptance:** V07 and V09 distinguish proven empty scope, unresolved ownership and absent usage coverage.

#### Pull recovery

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-recovery`

Subscriptions MUST expose authorized pull recovery for facts, bindings, membership, due groups and attribution. Reads preserve a consistent paging boundary, detect expired cursors and prevent older replacements from overriding newer state. Missing events cannot prevent discovery.

- **Consumers:** Rating and Billing.
- **Contract:** [Recovery](features/09-consumer-contracts.md#38-recover-gaps-cursor-loss-and-retained-history).
- **Acceptance:** V10 covers never-delivered events, late corrections, cursor loss and tenant isolation.

#### Requested fact intake

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-fact-intake`

Subscriptions writes each fact version as a `subscriptions-rating/1` document (SUB-D-117) and appends it to its recovery journal (SUB-D-120). When its events are enabled, it also delivers the whole document as `data.fact` of `BillableFactPublished` (SUB-D-121). Rating owns the reader of that profile. The steps below are Rating's: they are requests until Rating ratifies them in its own register (SUB-D-92), and they gate the profiles that need Rating's valuation under S-U08.

- **Event consumer.** Rating is asked to consume `billable_fact_published` on the Subscriptions topic and to hand `data.fact` to its fact intake as it is, with the producer that the broker verified on the envelope as the event origin. Rating is asked to adopt the fact source among its event contracts and to enable that source for the producer identity the broker authenticates for Subscriptions. Delivery is at least once: one fact version can arrive twice, under two event IDs (SUB-D-121), and Rating is asked to take a repeat of a version it holds, with the same payload digest, as a replay.
- **Recovery pull.** Rating is asked for a recovery adapter over the Subscriptions reads of feature 09 (`period_facts`, `facts` and the journal catch-up), with the pull as its origin, so that a fact whose event is lost, or was never enabled, still reaches Rating.
- **Digest verification.** Rating is asked to verify `payload_digest` with the codec of SUB-D-116, the SHA-256 of the canonical JSON of the document without `payload_digest`, instead of checking only its form.
- **Billing terms source.** `billing_terms` is the frozen billing terms record as stored: its `source` is a tagged object `{kind: …}`, and the record carries its `digest`. Rating's reader accepts that form and reads only `anchor_at`. Rating is asked to agree the `source` form with Subscriptions before it starts to interpret `billing_terms`.
- **Fixture conformance.** Subscriptions keeps the `subscriptions-rating/1` documents it writes as fixtures (SUB-D-117): the versions of an activation (`active`), of a suspend (`suspended`), of an ended subscription (`ended`), zeroed versions (`zeroed`), the versions of repaired periods (`repaired`), and the binding documents (`bindings`). Rating is asked to read each of them in its own fact-intake test and to accept it, so a change on either side that breaks the contract fails in Rating's tests.

- **Consumer:** Rating.
- **Contract:** [Commercial fact envelope](DESIGN.md#commercial-fact-envelope), [events and topics](DESIGN.md#events-and-topics), [recovery](features/09-consumer-contracts.md#38-recover-gaps-cursor-loss-and-retained-history).
- **Acceptance:** facts from an activation, a lifecycle change, a repaired period and a zeroed version reach Rating once through the event and again, unchanged, through the pull; a lost event is recovered by the pull; an event delivered twice is taken once; a changed digest under one version is a conflict; Rating's intake test accepts every fixture document; Rating ratification under S-U08.

#### Requested valuation rules

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-valuation-rules`

Subscriptions supplies amount-less facts with immutable accepted terms, term slices, lot allocation and interval evidence; Rating and Pricing own the valuation. The rules below are what Subscriptions requires of that valuation so that its facts are priced as accepted (SUB-D-75). They are requests until Rating and Pricing ratify them in their own registers; the affected profiles stay gated under S-U08 until then.

- **Recurring flat and pooled period floor.** (SUB-D-46): A recurring flat applies once per component obligation and covered time under identical applicable terms, without seat/lot multiplication. Different simultaneous flat tariffs are unsupported; authorized sequential terms use separate slices. Recurring proration and supported pooled period floors use exact covered UTC seconds divided by declared billing-period UTC seconds. Sum eligible base amounts before one floor per price/subscription/period/currency/scale; use interval union, not overlapping sums.

  Different price IDs retain separate floors. Cross-fact aggregation requires an owner contract: neither independent per-fact floors nor a general admission ban is selected; until it exists, a configuration that binds one minimum-bearing price on two facts of the same subscription and period refuses before effects. Billing rounds the financial result.

- **Per-resource minimum.** (SUB-D-47): Usage tariffs can require a separate minimum for each resource. Rating must preserve the resource identity; invoice lines and purchase lots cannot substitute for it. The pooled subscription profile also remains valid for its accepted terms.

- **Usage kind and resource group.** (SUB-D-48): Minimum policy distinguishes eligible usage from target identity: for example, storage for one container, traffic for another, or storage pooled across a node group. A node group is not a BillingGroup, component, lot or invoice line. Historical group membership and resource evidence must support valuation.

- **Tariff-selected scope.** (SUB-D-49): Individual and group minimum scopes are alternatives for the same eligible usage under one accepted tariff. Apply the selected scope once; do not stack individual and group floors or infer a hierarchical residual top-up. Different usage kinds can select different scopes. Preserve the selection in accepted terms independently of invoice grouping.

- **Monetary usage minimum.** (SUB-D-50): Rate eligible usage, then apply `max(0, applicable_minimum_amount - eligible_base_amount)` once per selected target/window. Group scope sums eligible member amounts before the floor; individual scope evaluates each resource. Minimums do not inflate metered quantity.

- **Minimum window.** (SUB-D-51): The tariff selects hourly or subscription billing-period monetary minimums. Preserve the window and amount in accepted terms. An hourly minimum does not determine invoice frequency or imply simultaneous hourly and period floors.

- **Partial hour.** (SUB-D-52): Any eligible partial hour receives the full hourly monetary minimum once per selected target/window, without time proration or lot/member multiplication. Fifteen eligible minutes with a 1 EUR hourly minimum retain a 1 EUR floor; a larger rated base remains payable at its calculated amount. Completeness evidence remains required.

- **Calendar hours.** (SUB-D-53): Hourly windows follow calendar-hour boundaries. Service from 10:50 to 11:10 spans two minima, each compared with that hour's eligible base. Excess in one hour cannot offset another. Half-open attribution does not charge the next hour solely because service ends on its boundary. Exact timezone and lifecycle/group-change mapping require the owner contract.

- **Partial billing period.** (SUB-D-54): Individual/group billing-period tariffs explicitly select full or proportional minimum; no implicit default is selected. Preserve the mode in accepted terms. A 30 EUR minimum over 30 days with 10 eligible days gives a 30 EUR floor in full mode or 10 EUR in proportional mode. Eligible-duration/group-membership evidence and exact SDK mapping remain integration work. Existing pooled-period terms and the full partial-hour rule retain their own semantics.

- **Consumer:** Rating and Pricing.
- **Contract:** [Rating result contract](DESIGN.md#rating-result-contract), [valuation inputs](features/08-commercial-facts.md#33-derive-stable-facts-and-complete-accepted-term-slices).
- **Acceptance:** the minimum-fee, partial-hour and partial-period fixtures of feature 08; Rating and Pricing ratification under S-U08 ([proposal](https://github.com/constructorfabric/gears-rust/pull/5390#issuecomment-6360750023)).

#### Requested trial-credit accounting rules

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-trial-credit-accounting`

Subscriptions carries the grant reference and digest in preparation posture, records actual trial start, original expiry `E`, actual paid transition `T_paid` when present and each component's rating/allocation boundary `B`, and classifies served time by those boundaries and keeps its facts amount-less. Everything below is what it requires of Billing for the promotional grant (SUB-D-75); it is a request until Billing ratifies it in its own register, and the credit-trial profile stays gated under S-U15 until then.

Trial credit MUST use shared accounting with a distinct durable customer grant tied to accepted acquisition/episode and immutable policy/applicability. Billing is required to own promotional trial-credit issuance/history/remaining balance and allocation/correction; S-U15 must establish its exact provider and Ledger integration, authoritative versioned outcomes and invoice coordination. Billing is required to consume canonical full Rating replacements, preserve prior accepted allocation/version lineage and adjust the same grant once under original scope/currency/terms. Repeated accepted versions MUST be no-ops; conflicting version/content MUST refuse or recover.

No independent delta-delivery contract or inline rewrite of posted financial history is introduced. Multiple eligible items share one grant; no cloned full bonus, implicit all-product/FX scope or new setup-charge trigger is allowed. Pricing stores terms, not balances; Subscriptions facts remain amount-less with relevant policy/grant evidence references, not bonus amounts or balance math. Trial bonus is neither customer prepayment nor authorization for postpay service. Current Ledger cash-funded grant semantics MUST NOT be repurposed as an existing free-bonus capability or supplied with fabricated settled cash; necessary financial postings require the agreed Billing/Ledger path.

A downward correction at the same accepted tariff MUST restore the released eligible amount on the same grant exactly once while that grant remains valid and applicable to an active trial or eligible paid usage after early guarded conversion, only until the same original expiry: illustratively, grant 100 minus usage value 30 leaves 70, and corrected value 25 leaves 75. After grant expiry, Billing is required to still correct history/accounting and preserve the original expiration: historical net consumption is 25 and conceptual remaining is 75, but spendable credit is 0.

Correction MUST NOT extend the trial or create a new grant/top-up, cash refund, repricing or access revival. The numbers are illustrative semantic outcomes, not defaults or prescribed balance fields. A later correction MUST NOT automatically reopen a trial already ended by exhaustion, even before original expiry. Illustratively, full consumption 100 corrected to 95 restores 5 in accounting but grants no service/spend permission for the closed episode and MUST NOT reverse an applied paid conversion.

A correction MUST NOT end a still-active trial or prevent restoration of credit to the same grant while it remains valid and applicable, until the original expiry. V1 has no reactivation of a closed trial episode, rollover past original expiry or regrant on the same subscription. A later acquisition after a trial is a fresh paid purchase; repeat trials are deferred; its guarded routing and owner contracts remain required. Positive corrections of eligible pre-boundary trial expenses follow the platform-liability rule below; correction/liability cases outside that scope remain gated.

On an early valid guarded trial-to-paid transition, Billing is required to retain unused monetary trial credit on the same grant for eligible paid-plan expenses until the same original expiration, under unchanged currency/product/item/charge-kind and other accepted restrictions. Closing the trial episode through that conversion MUST NOT itself make the retained grant unspendable. Illustratively, a 100-credit/14-day grant with 60 left on day 5 keeps that 60 eligible only until original day 14; conversion issues no new grant, top-up or duration.

Expired grants are not revived. Full Rating replacement/correction lineage MUST continue for eligible paid usage using its frozen agreed tariffs and original grant applicability. A paid-plan SKU outside that applicability MUST NOT spend the balance or cause it to be erased; no product-scope or FX expansion follows. Credit alone grants no access or paid-transition permission, and MUST NOT be assumed to satisfy prescribed upfront payment unless the agreed coverage/settlement contract allows it. Stacking and new-period rollover/regrant are refused in v1; payer transfer is deferred and cannot move this grant. Exact S-U15 provider/integration/coverage contracts remain external delivery gates.

Credit trials MUST retain legitimate served/usage facts valued at ordinary tariffs before paid-target conversion. Rated gross value, bonus coverage, platform-borne amount, customer obligation, access-stop evidence and the successor paid boundary are distinct. Expiry-no-debt/no-paid-start rules do not suppress trial evidence needed for drawdown. Overage or excluded charges require an agreed paid basis and guards. For eligible expenses before the component's paid rating/allocation boundary `B` (SUB-D-67), the platform MUST bear full rated value beyond bounded, applicable bonus coverage, including positive corrections and in-flight overrun. `B` MUST be `T_paid` for non-hourly components or the first UTC-hour boundary at or after `T_paid` for hourly components.

Billing MUST NOT create customer trial-overage debt for this excess. Illustratively, remaining valid bonus 2 and eligible pre-`B` usage value 5 means bonus coverage 2 and platform-borne 3. For hourly components this applies throughout `[T_paid, B)`, even though status/access are already paid. Free-period hourly components keep their zero binding until `B`. Credit components use only bounded, applicable bonus for service strictly before original exclusive expiry `E`; exhausted or expired credit covers nothing, and the platform bears the residual until `B`. From `B`, residual follows accepted paid-plan/settlement terms after any still-valid eligible bonus coverage. Non-hourly components use those paid terms from `T_paid`.

Rating and Billing MUST use the same per-component `B`. Facts and recovery MUST preserve actual `T_paid`, original `E`, each component's `B`, served extents, policy version and grant/result lineage; late events and corrections MUST reuse that evidence. At an exact hour `B = T_paid`; no extra hour is added. Hourly tariff slices change at `B`, without shifting actual service or access boundaries. A scalar hourly peak MUST NOT be split at `T_paid` or prorated by minutes. Grant-eligibility/stopping-delay allocation still requires supported evidence; missing evidence MUST leave the affected allocation pending, not invent bonus coverage or customer debt.

Expiry without a valid paid conversion MUST NOT create `T_paid`, `B` or a paid successor binding. Revocation starts at original `E`; actual stopping-delay usage until access stops, including beyond the next hour, MUST remain platform-borne, spend no trial credit and not be classified as a trial expense. Hour rounding MUST NOT extend trial access or grant validity. These component boundaries and liability rules require S-U08/S-U15 owner adoption and executed TP-C27–TP-C29 before enablement.

Rating is required to preserve actual gross usage/value and Billing is required to distinguish bounded bonus coverage, platform-borne amount and customer obligation; no negative bonus, unlimited top-up or fabricated coverage of 5 is allowed. A canonical full replacement correcting illustrative eligible pre-boundary gross value from 90 to 110 against a grant of 100 MUST allocate bonus 100, platform 10 and customer 0, retaining original source/version and tariff lineage. Replay MUST NOT repeat the adjustment, issue a grant or reopen an exhausted episode.

Erroneous billed trial tail MUST use existing idempotent Billing correction/remedy paths, not an additional goodwill payment. This selects neither a refund mechanism nor a Ledger journal/tax schema, and waives no prior legitimate debt, explicit retention obligation, excluded charge kind or unrelated recovery/remedy obligation. Before credit launch, S-U15 MUST demonstrate this coverage boundary and ratify exact evidence/provider/Ledger, detection/enforcement/recovery and remaining liability contracts. No instantaneous stop or unlimited free operation is promised; unknown balance is neither zero/exhaustion nor spend permission.

- **Consumer:** Billing, with Rating and Ledger where named.
- **Contract:** [trial coverage](features/06-trials.md#credit-preparation-and-recovery), [trial terms](DESIGN.md#336-trial-terms-and-provider-evidence).
- **Acceptance:** TP cases, including mixed component boundaries and expiry/correction cases TP-C27–TP-C29, and the illustrative 100/30/25, 60-on-day-5 and 90→110 corrections; Rating/Billing adoption under S-U08/S-U15.

### 2.4 Commercial posture and retention

#### Commercial entitlement posture

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-entitlement-posture`

Subscriptions MUST publish `CommercialPostureV1` with its committed revision. Initial activation requires held `ReadinessObservationV1` for the exact operation, preparation revision and complete manifest. Applied activation and terminal non-activation share one durable fence and remain recoverable. A local receipt alone does not prove usable access.

- **Consumer:** Entitlements.
- **Contract:** [Commercial posture and readiness](features/05-entitlements.md).
- **Acceptance:** V11 and V19 cover replay, lost outcomes, held preparation and stale execution.

#### Resource and evidence retention

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-retention`

Subscriptions MUST publish explicit stop, retention/export, hold and deletion obligations. Confirmed charging cutoff, applied cancellation, actual resource stop and deletion are distinct. Suspension preserves data. Archive cannot erase unresolved resource or financial obligations.

- **Consumers:** Entitlements and Billing.
- **Contract:** [Renewal and offboarding](features/04-renewal-retention.md).
- **Acceptance:** V13–V14 and RO cases cover delayed stop, frozen terms, holds, notice-free acceptance and separate financial retention.

### 2.5 Provider prerequisites and bilateral acceptance

Each enabled profile requires agreed provider versions, an authorization and recovery mapping, and executed producer/consumer cases. The table names the dependency and its acceptance evidence. Detailed behavior belongs to the linked contract. An unavailable required provider blocks that profile; a test double does not establish production readiness.

| ID | Owner | Required contract | Acceptance evidence / dependent capability |
| --- | --- | --- | --- |
| S-U01 | Orders / Workflow / Pricing / Products | [Protected acquisition](DESIGN.md#332-pricing-acceptance-and-accepted-time): durable accepted future terms, an AP03-aware fresh Pricing execution check with complete result/Orders mappings, capture/cancel ordering, trial-admission reservation at confirmation (AP08), attempt association, recovery and an agreed preparation-preserving pause/resume protocol (SUB-D-59; Orders adoption pending). | AP-C01–AP-C16; acquisition, initial activation and supported successor attempts. |
| S-U02 | Infrastructure Resource Manager (expected default) or another resource evidence provider, as a `historical_inventory` plugin instance (SUB-D-74) | [Historical inventory](features/09-consumer-contracts.md#39-replace-attribution-without-erasing-financial-history), including deleted resources, lifecycle barriers and typed gaps. | V09; attribution and sealed ownership scopes. |
| S-U03 | Usage emitter / Products / Rating, as a `usage_coverage` plugin instance (SUB-D-74) | [Usage coverage](features/09-consumer-contracts.md#39-replace-attribution-without-erasing-financial-history), independent of ownership and collector progress. | V07 and V09; final usage valuation and invoicing. |
| S-U04 | Entitlements / Subscriptions / Orders and Workflow for held preparation | [Posture and held readiness](features/05-entitlements.md#initial-activation-readiness-contract), exact terminal resolution, timeout and unknown-outcome recovery. Entitlements owns adapter conformance, including pending-conversion restriction through full snapshots/resume and separate desired/observed revocation (TP-C30–TP-C32). Initial preparation is not required for the local `no_preparation` profile (SUB-D-70); ongoing provider restrictions still require their applicable posture contract. The aggregate readiness result is read by operation and posture revision, and an inbound observation wakes the activation driver (SUB-D-87); until the preparation provider is configured, the `entitlements` profile refuses `capability_closed` (SUB-D-85). The lifecycle posture owner applies a posture only when its revision is newer than the last it applied, is idempotent per subscription and revision, and retries its own failed apply: the gear observes a failed posture again, never sends a taken revision to apply twice, and reports a row overdue as the escalation (SUB-D-106). | V11 and V19; an agreed count/elapsed-age limit and Orders-authorized closing action for held preparation, including a paused attempt at that limit (SUB-D-57); entitlement-dependent activation and lifecycle application. |
| S-U05 | Entitlements / Products / Billing / Rating | [Offboarding](features/04-renewal-retention.md): confirmed cutoff, actual stop, retention and effect-owner hold/delete fencing. | V13 and RO cases; safe offboarding and financial closeout. |
| S-U06 | Contracts / Products | [Renewal terms](features/04-renewal-retention.md): explicit bound-term precedence, frozen evaluated deadlines and next-renewal decisions. Current renewal, continuation and retention terms must require no notice or separate assent; refuse unsupported required behavior at acceptance (SUB-D-69). The owner contract must define service posture, billing treatment and bounded recovery/exit after a term boundary during a Contracts outage; missing policy keeps that profile disabled. Pending age alone does not close this gate. | V14 and RO cases; contract-driven renewal and retention. |
| S-U07 | Financial admission owner (a `financial_admission` plugin instance, SUB-D-74) / Orders / Payments / Billing | [Financial admission](DESIGN.md#3311-financial-admission-integration-contract): retained payment or future postpay coverage, explicit dispositions, capture and recovery fences; an explicit trial-conversion collection request/recovery contract with a stable occurrence and an expiry/closure capture-start fence; stable collection-obligation identity, correction/reissue lineage and cause-specific ordered outcomes for overlapping [collection grace](features/04-renewal-retention.md#7-renewal-and-offboarding-integration-contract). | FA-C01–FA-C12, TP-C33, V18 and RO-C20–RO-C24; financially guarded operations without duplicate collection or live reassessment of an accepted purchase. |
| S-U08 | Pricing / Rating | [Rating contract](DESIGN.md#rating-result-contract): exact accepted bindings, concurrent lots, one-time occurrence/basis, supported minimum-fee scopes and complete replacements. Current Rating documents still describe a different continuation and one-time valuation; the owner alignment is tracked in the [Subscriptions proposal for the Rating contract](https://github.com/constructorfabric/gears-rust/pull/5390#issuecomment-6360750023). Fact intake by event and pull ([`cpt-cf-bss-subscriptions-upreq-fact-intake`](#requested-fact-intake)), whole-second accepted instants and an accepted suspension posture (below the table). | Rating acceptance cases; supported recurring, usage, one-time and quantity-change valuation. |
| S-U10 | Orders / Products / Pricing / financial owner | [Occupancy contract](DESIGN.md#accepted-acquisition-evidence): versioned scope/limit, protected claims and atomic conversion/release. | V03; protected acquisition, supported key changes, and a new origin version of a line whose earlier version's subscription has ended, which `version_amendment` refuses until then (SUB-D-100, SUB-D-102). |
| S-U11 | Product / SRE / consumers | [Measured recovery and workload evidence](features/09-consumer-contracts.md#39-replace-attribution-without-erasing-financial-history) against PRD budgets. | V16; production readiness. |
| S-U12 | Pricing / product policy owners | [Trial terms](DESIGN.md#336-trial-terms-and-provider-evidence): immutable variant, duration, conversion lead time (SUB-D-65), exhaustion action for credit trials (SUB-D-66), applicability, initial eligibility and accepted paid target. | TP cases; free-period and credit trials. |
| S-U13 | Billing / Rating / Ledger | [Financial replacements](DESIGN.md#financial-consumer-boundary): exact seals, invoice-version fencing, advances, final rounding and recoverable corrections, including historical unresolved intervals that survive cancellation. | Joint seal, replacement and financial-recovery cases; invoicing and closeout. |
| S-U15 | Billing / Rating / Pricing / Ledger | [Trial coverage](features/06-trials.md#credit-preparation-and-recovery): one reserved grant effective at activation, original expiry, allocation and correction. | TP cases; credit trials. Free-period trials do not require a promotional-grant provider. |
| S-U16 | Pricing / Orders Changes | [Change-delta acceptance](features/03-changes.md#apply-one-atomic-changecomposition): a delta accepted per change order/version/line against the target subscription and expected revision, bindings pinned at submit, frozen instant at the applied commit, hold window and expiry, and reuse after a definitively unapplied attempt with immutable Pricing revalidation when the expected revision changes (SUB-D-61). Revalidation binds the acceptance ID/digest to the new revision without repricing or extending expiry; absent or refused evidence blocks the retry. | V06 and AC 60; order-originated increases and additions. |
| S-U17 | Settings Service / governance owner | [Policy set](DESIGN.md#42-gts-identifiers): the Settings key `gts.cf.core.settings.setting_type.v1~cf.bss.subscriptions.lifecycle_policy.v1~` with `value_type_id = gts.cf.bss.subscriptions.policy_set.v1~`, platform and seller-tenant scopes, resolved value with version and provenance, authorized change, and governance minima as floors (SUB-D-72). It includes the activation profile that a seller tenant sets for a resource tenant: service profile, preparation duration and `before_apply` hook budget (SUB-D-85). | V14 and RO cases; activation, preparation, renewal, grace, retention and purge require the applicable resolved set. New seller/configuration policy versions refresh scoped profile resolution; admitted operations retain their original owner and evidence. |
| S-U18 | Serverless runtime / task engine | [Lifecycle hooks](DESIGN.md#42-gts-identifiers): execution of `lifecycle_hook` plugin instances at `before_apply` for veto-eligible operations (allow or typed veto within the operation budget, fail closed) and `after_applied` (at-least-once from the outbox). Authenticated mandatory expiry, exhaustion-end, compensation and cleanup/closure skip veto hooks while retaining normal guards; caller-supplied reasons cannot select that exception. Final activation veto and timeout follow the retained-attempt recovery contract, with the hook snapshot schema and the registry (SUB-D-73). | Hook veto, timeout and at-least-once delivery cases; until delivered the hook stage is a no-op. |
| S-U19 | Accounts (commercial accounts, arrangements and grants, [proposed](../../accounts/docs/DESIGN.md)) | [Accounts consumer contract](DESIGN.md#tenant-isolation-delegation-proofed-cross-tenant) to Subscriptions (SUB-D-124): narrow roles for the gear's service principal to authorize a use, record its outcome, release, end and read status; `authorize_use` for renewal, quantity increase and payer or account transfer, with the `use_version` that Subscriptions allocates; the grant evidence, payer account and debtor that the billing assignment records; and the batched status read for the payer leg view. Subscriptions records the assignment; Accounts owns none. The first purchase is the use of Orders, and its grant is evidence that the create carries. References to the Accounts binding stand beside the Tenant Resolver topology for commercial account and OrgTier roll-ups. Until Accounts is delivered, the relationship evidence of a purchase is the accepted Pricing receipt checked against the create (SUB-D-108), with one authorization per tenant axis (SUB-D-83). Tenant ancestry is never relationship evidence, and no create waits on Accounts. | Joint grant, revocation, replay and use-version cases; renewal, increase and transfer grants, assignment segments with the chain version, the payer leg view, `assignment_status`, the readiness causes and payer relationships beyond the accepted receipt. Payer and account transfer stay deferred (SUB-D-113). |
| S-U20 | Authz Resolver: the decision point plugin that the platform deploys, and the platform authorization owner | [Single-target scopes](DESIGN.md#tenant-isolation-delegation-proofed-cross-tenant) (SUB-D-125): the deployed decision point answers a root-only evaluation for target T with a scope fenced to T on every branch, and the authorization owner confirms the [commercial model §3.4](../../../../docs/arch/bss-commercial-model/DESIGN.md#34-internal-dependencies) contract, the acceptance condition of ADR 0002. That contract covers narrow roles assigned at the platform root, the returned-scope guard, the target's lifecycle check, an explicitly authorized internal service path across a self-managed barrier, and an authorized tenant roster for discovery in pages of at most 200 (SUB-D-123). The deployed decision point answers a grant anchored at R above T with `owner_tenant_id IN [R]`; until it answers fenced to T, the gear runs the interim of SUB-D-125: it builds the command scope `owner_tenant_id IN [T]` from that allow, records R as the scope proof (SUB-D-83, SUB-D-112) and refuses a service caller whose tenant is behind a self-managed barrier. | A contract test of the deployed plugin for a target below the grant's anchor; overbroad and unfenced answers refused by the guard; self-managed targets on the internal path; roster discovery; tests at 10,000 and 10,001 tenants and a target of 100,000. Ends the interim of SUB-D-125 and the discovery warrant of SUB-D-123. |
| S-U21 | Orders | [Orders, terms and Subscriptions contracts](../../../../docs/arch/bss-commercial-model/DESIGN.md#38-orders-terms-and-subscriptions-contracts) (SUB-D-124): Orders consumes the termination receipt of Subscriptions, keyed by the admitted order version and the line or leg, and ends its grant from it; it marks an attachment (upstream) order by its category, so that a create refuses it; it passes the source user actor in the create, the receiver commands and the reservation commands (SUB-D-112); and it uses the same versioned occupancy key as Subscriptions, switched together under one gate after an empty collision report (SUB-D-63). | Joint termination-handoff, attachment-refusal and provenance cases; the collision report and the coordinated key switch. Ends the stored payer in the occupancy key (SUB-D-63) and the missing source user actor (SUB-D-112). |

Preparation policy is snapshotted on each attempt. Elapsed held age continues while its technical deadline is paused. Orders/Workflow, Subscriptions and Entitlements must agree an action that bounds the existing hold, including revoke and recovery; an alert or refusal of new preparations alone is insufficient. Subscriptions cannot invent a TTL or release an Orders hold. The held-preparation profile remains unavailable until S-U01/S-U04 prove this behavior. Applicable S-U01/S-U04/S-U07/S-U13 mappings must prove bounded preparation, deadline/activation arbitration and platform accounting for authorized pre-activation use. Unknown effects remain recoverable; neither a timeout nor delayed publication releases held preparation or spends trial credit. See [readiness](features/05-entitlements.md#initial-activation-readiness-contract) and [trial coverage](features/06-trials.md).

**Whole seconds and suspension posture (S-U08).** The `subscriptions-rating/1` profile reads only whole UTC seconds. A frozen billing `anchor_at` or accepted start finer than a second blocks the subscription's periods (`sub_second_instant`, SUB-D-115), and Subscriptions never truncates it. Orders and Pricing are asked to send and freeze the requested start and `anchor_at` at whole UTC seconds. Accepted terms carry no suspension bill/waive posture in this version, so a suspended span is published as an `unresolved` slice and Rating holds it pending (SUB-D-117). Pricing is asked to carry an accepted suspension posture in the accepted terms, and Rating to value it, before suspended time can be priced.

## 3. Priorities

All incoming requirements above are `p1`. Enable only profiles within [PRD scope](PRD.md#11-purpose) whose applicable provider contracts pass. Deferred capabilities create no current delivery dependency.

## 4. Traceability

- [PRD](PRD.md): `Covers` maps incoming requirement IDs to product requirements.
- [DESIGN](DESIGN.md), [features and acceptance cases](DECOMPOSITION.md): architecture, implementation allocation and V scenarios.
- [DECISIONS](DECISIONS.md): commercial and ownership decisions.
