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
| Orders Lifecycle, Workflow and Changes | Recoverable operations, order identity, occupancy and atomic composition changes |
| Rating | Accepted terms, period facts, historical attribution, scope proofs and recovery |
| Billing | Commercial group membership, exact seals, source lineage and service boundaries |
| Entitlements | Commercial posture, preparation context, disposition and authoritative activation resolution |

## 2. Requirements

### 2.1 Orders operations

#### Durable operation protocol

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-order-intents`

Subscriptions MUST expose separate draft creation and activation, bind operations to the acquisition origin identity (`AcquisitionOriginV1`, SUB-D-71; for Orders: order, version, attempt, generation, line), and distinguish acceptance from application. Orders is the origin provider: it owns accepted purchase evidence, reservation at confirmation, capture ordering, compensation, hold/unpause/revoke and remediation of a closed attempt. Orders MUST treat `preparing` as a non-served, non-terminal status that counts as a draft for occupancy (SUB-D-58); the Orders contract D-242 needs this alignment. Replay, withdrawal, status lookup and uncertain-create settlement MUST preserve one acquisition and its financial obligations. The create business key excludes generation; its result or no-create tombstone covers all generations of the same attempt. The reservation key excludes both attempt and generation and has a separate release tombstone. Attempt settlement MUST NOT release that reservation. Exact lookup MUST expose the stable create outcome separately from receiver generation state, including recovery after a changed envelope conflicts with a cached transport request. A successor attempt requires fenced predecessor settlement. Orders MUST send a hold as a `pause` command and its release as `unpause` per receiver generation, and a revocation as `revoke`; Subscriptions keeps the receiver ledger `(order, version, attempt, generation, line)` with the states open, paused, revoked and settled, and a paused generation can neither admit new preparation nor commit active (SUB-D-59). Same-generation `unpause` MUST continue the existing preparation with its retained readiness and remaining deadline. This resume protocol is a pending owner request: current Orders instead issues a successor generation and leaves the old grant fenced. A successor grant alone MUST NOT take over paused preparation, reset its deadline or authorize cleanup. S-U01 remains closed until Orders adopts the requested protocol or both owners replace it with an explicit preparation-transfer contract and executed joint cases. Subscriptions runs the Orders D-190 fresh `check_fulfilment` before activation as a refusal-only check (expired acceptance, explicit price closure, retired SKU; SUB-D-60); it never reprices the accepted purchase. Held entitlement preparation and reservation-backed payment capture MAY precede T; neither establishes `serviceActivatedAt`, service-period accrual or a billable service fact. The requested Orders wording must distinguish served service from this preparation. The current Orders sentence is not yet aligned; adopting this distinction and `preparing` is an S-U01 prerequisite.

- **Consumer:** Orders Lifecycle and Workflow.
- **Contract:** [Activation](DESIGN.md#332-pricing-acceptance-and-accepted-time), [foundation](features/01-foundation.md).
- **Acceptance:** V01–V02 and AP-C01–AP-C14 cover lost replies, cancellation, partial order fulfillment, recovery and pause/resume races, including pause before preparation admission and preservation of an existing preparation and its remaining deadline.

#### Occupancy and atomic entry

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-occupancy`

Subscriptions MUST expose the authoritative scope key, limit and policy version with separate active, retained-suspended and order-reserved counts. Admission and claim conversion MUST be atomic. Confirmation reserves before capture; activation or unchanged-key resume consumes its own right once. Paid or uncertain claims survive technical expiry. Lowering the limit does not evict protected claims. The read returns the claim set, one claim per distinct key of the resulting composition; a component addition claims new keys without releasing existing ones, so an `add_component` whose new key is held by another subscription is refused at the same commit and visible to the Orders gate before approval (SUB-D-63).

- **Consumer:** Orders Lifecycle and Changes.
- **Contract:** [Foundation](features/01-foundation.md), [activation](DESIGN.md#332-pricing-acceptance-and-accepted-time).
- **Acceptance:** V03 covers concurrent admission, suspension, reservation conversion, failed key movement and fenced release.

#### Atomic composition change

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-composite-change`

Subscriptions MUST apply an admitted multi-item change to one subscription in one transaction against the expected revision. Validate the resulting bounds, eligibility and occupancy together. Increases and additions require an active subscription, or a draft subscription once the S-U01 draft-change profile is enabled; suspended purchases refuse at submission and commit. Unsupported dates or items refuse the whole request before effects. A change order that passes the Orders submit gate (which admits any non-terminal target) and reaches the Subscriptions active-target, suspended-purchase or closed draft-change guard receives a typed refusal that names the guard; Orders MUST hold the change order for operator resolution (a separate guarded resume, or the S-U01 draft profile once enabled) instead of treating approval as admission. Orders MUST reject a requested effective date on a change line before approval in this version (CHG-S5 refused); an absent date means application at the Subscriptions commit, never the day of submission. This is the required owner behavior, not a claim that the current Orders contract already enforces it. A change that still arrives with a requested date receives the typed future-boundary refusal. The composition read exposes each line's accepted binding reference so the Orders gate can refuse an increase whose delta binding differs from the existing quantity's binding while S-U08 is closed.

The current Orders Changes contract still uses non-terminal admission, failure/re-derivation and requested dates. The owner must adopt the target-state guards, operator-hold outcome and date rejection above; S-U16 stays closed until bilateral cases prove the mapping.

- **Consumer:** Orders Changes.
- **Contract:** [Composition changes](features/03-changes.md).
- **Acceptance:** V06 covers suspension races, stale revisions, invalid bounds and atomic refusal.

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

### 2.2 Pricing and commercial identity

#### Accepted commercial inputs

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-accepted-bindings`

Subscriptions MUST retain immutable accepted terms and purchase-lot provenance. Activation executes the accepted purchase without live catalog re-admission. Renewal and mid-term continuation require accepted authority and complete effective-dated slices. Missing continuation evidence produces an explicit unresolved fact version; it never selects current prices or silently values the gap at zero.

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

Subscriptions carries the grant reference and digest in preparation posture, records actual T, original expiry and the applied paid boundary, classifies served time by those boundaries and keeps its facts amount-less. Everything below is what it requires of Billing for the promotional grant (SUB-D-75); it is a request until Billing ratifies it in its own register, and the credit-trial profile stays gated under S-U15 until then.

Trial credit MUST use shared accounting with a distinct durable customer grant tied to accepted acquisition/episode and immutable policy/applicability. Billing is required to own promotional trial-credit issuance/history/remaining balance and allocation/correction; S-U15 must establish its exact provider and Ledger integration, authoritative versioned outcomes and invoice coordination. Billing is required to consume canonical full Rating replacements, preserve prior accepted allocation/version lineage and adjust the same grant once under original scope/currency/terms. Repeated accepted versions MUST be no-ops; conflicting version/content MUST refuse or recover.

No independent delta-delivery contract or inline rewrite of posted financial history is introduced. Multiple eligible items share one grant; no cloned full bonus, implicit all-product/FX scope or new setup-charge trigger is allowed. Pricing stores terms, not balances; Subscriptions facts remain amount-less with relevant policy/grant evidence references, not bonus amounts or balance math. Trial bonus is neither customer prepayment nor authorization for postpay service. Current Ledger cash-funded grant semantics MUST NOT be repurposed as an existing free-bonus capability or supplied with fabricated settled cash; necessary financial postings require the agreed Billing/Ledger path.

A downward correction at the same accepted tariff MUST restore the released eligible amount on the same grant exactly once while that grant remains valid and applicable to an active trial or eligible paid usage after early guarded conversion, only until the same original expiry: illustratively, grant 100 minus usage value 30 leaves 70, and corrected value 25 leaves 75. After grant expiry, Billing is required to still correct history/accounting and preserve the original expiration: historical net consumption is 25 and conceptual remaining is 75, but spendable credit is 0.

Correction MUST NOT extend the trial or create a new grant/top-up, cash refund, repricing or access revival. The numbers are illustrative semantic outcomes, not defaults or prescribed balance fields. A later correction MUST NOT automatically reopen a trial already ended by exhaustion, even before original expiry. Illustratively, full consumption 100 corrected to 95 restores 5 in accounting but grants no service/spend permission for the closed episode and MUST NOT reverse an applied paid conversion.

A correction MUST NOT end a still-active trial or prevent restoration of credit to the same grant while it remains valid and applicable, until the original expiry. V1 has no reactivation of a closed trial episode, rollover past original expiry or regrant on the same subscription. A later acquisition after a trial is a fresh paid purchase; repeat trials are deferred; its guarded routing and owner contracts remain required. Positive corrections of eligible pre-boundary trial expenses follow the platform-liability rule below; correction/liability cases outside that scope remain gated.

On an early valid guarded trial-to-paid transition, Billing is required to retain unused monetary trial credit on the same grant for eligible paid-plan expenses until the same original expiration, under unchanged currency/product/item/charge-kind and other accepted restrictions. Closing the trial episode through that conversion MUST NOT itself make the retained grant unspendable. Illustratively, a 100-credit/14-day grant with 60 left on day 5 keeps that 60 eligible only until original day 14; conversion issues no new grant, top-up or duration.

Expired grants are not revived. Full Rating replacement/correction lineage MUST continue for eligible paid usage using its frozen agreed tariffs and original grant applicability. A paid-plan SKU outside that applicability MUST NOT spend the balance or cause it to be erased; no product-scope or FX expansion follows. Credit alone grants no access or paid-transition permission, and MUST NOT be assumed to satisfy prescribed upfront payment unless the agreed coverage/settlement contract allows it. Stacking and new-period rollover/regrant are refused in v1; payer transfer is deferred and cannot move this grant. Exact S-U15 provider/integration/coverage contracts remain external delivery gates.

Credit trials MUST retain legitimate served/usage facts valued at ordinary tariffs before paid-target conversion. Rated gross value, bonus coverage, platform-borne amount, customer obligation, access-stop evidence and the successor paid boundary are distinct. Expiry-no-debt/no-paid-start rules do not suppress trial evidence needed for drawdown. Overage or excluded charges require an agreed paid basis and guards. For eligible accepted-trial expenses served before the authoritative applied paid boundary, the platform MUST bear full rated value beyond bounded bonus coverage, including positive corrections, errors and accounting/stopping-delay or in-flight overrun.

Billing is required not to create customer trial-overage debt for this excess. Illustratively, remaining bonus 2 and eligible trial usage value 5 means bonus coverage 2 and platform-borne 3. For usage actually served after valid guarded paid conversion, the residual 3 instead follows accepted paid-plan/settlement terms. Classification MUST use actual served time and the authoritative applied paid boundary, never rating/event/correction arrival; late pre-conversion facts remain trial liability, crossing usage MUST be split/attributed using supported evidence, and paid conversion MUST NOT be backdated to charge trial overrun.

Rating is required to preserve actual gross usage/value and Billing is required to distinguish bounded bonus coverage, platform-borne amount and customer obligation; no negative bonus, unlimited top-up or fabricated coverage of 5 is allowed. A canonical full replacement correcting illustrative eligible pre-boundary gross value from 90 to 110 against a grant of 100 MUST allocate bonus 100, platform 10 and customer 0, retaining original source/version and tariff lineage. Replay MUST NOT repeat the adjustment, issue a grant or reopen an exhausted episode.

Erroneous billed trial tail MUST use existing idempotent Billing correction/remedy paths, not an additional goodwill payment. This selects neither a refund mechanism nor a Ledger journal/tax schema, and waives no prior legitimate debt, explicit retention obligation, excluded charge kind or unrelated recovery/remedy obligation. Before credit launch, S-U15 MUST demonstrate this coverage boundary and ratify exact evidence/provider/Ledger, detection/enforcement/recovery and remaining liability contracts. No instantaneous stop or unlimited free operation is promised; unknown balance is neither zero/exhaustion nor spend permission.

- **Consumer:** Billing, with Rating and Ledger where named.
- **Contract:** [trial coverage](features/06-trials.md#credit-preparation-and-recovery), [trial terms](DESIGN.md#336-trial-terms-and-provider-evidence).
- **Acceptance:** TP cases and the illustrative 100/30/25, 60-on-day-5 and 90→110 corrections; Billing ratification under S-U15.

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
- **Acceptance:** V13–V14 and RO cases cover delayed stop, frozen terms, holds, notice prerequisites and separate financial retention.

### 2.5 Provider prerequisites and bilateral acceptance

Each enabled profile requires agreed provider versions, an authorization and recovery mapping, and executed producer/consumer cases. The table names the dependency and its acceptance evidence. Detailed behavior belongs to the linked contract. An unavailable required provider blocks that profile; a test double does not establish production readiness.

| ID | Owner | Required contract | Acceptance evidence / dependent capability |
| --- | --- | --- | --- |
| S-U01 | Orders / Workflow / Pricing / Products | [Protected acquisition](DESIGN.md#332-pricing-acceptance-and-accepted-time): durable accepted future terms, capture/cancel ordering, trial-admission reservation at confirmation (AP08), attempt association, recovery and an agreed preparation-preserving pause/resume protocol (SUB-D-59; Orders adoption pending). | AP-C01–AP-C14; acquisition, initial activation and supported successor attempts. |
| S-U02 | Infrastructure Resource Manager (expected default) or another resource evidence provider, as a `historical_inventory` plugin instance (SUB-D-74) | [Historical inventory](features/09-consumer-contracts.md#39-replace-attribution-without-erasing-financial-history), including deleted resources, lifecycle barriers and typed gaps. | V09; attribution and sealed ownership scopes. |
| S-U03 | Usage emitter / Products / Rating, as a `usage_coverage` plugin instance (SUB-D-74) | [Usage coverage](features/09-consumer-contracts.md#39-replace-attribution-without-erasing-financial-history), independent of ownership and collector progress. | V07 and V09; final usage valuation and invoicing. |
| S-U04 | Entitlements | [Posture and held readiness](features/05-entitlements.md#initial-activation-readiness-contract), exact terminal resolution, timeout and unknown-outcome recovery. Entitlements owns adapter conformance. Not required for the local `no_preparation` profile (SUB-D-70). | V11 and V19; an agreed limit on outstanding held preparation (SUB-D-57); entitlement-dependent activation and lifecycle application. |
| S-U05 | Entitlements / Products / Billing / Rating | [Offboarding](features/04-renewal-retention.md): confirmed cutoff, actual stop, retention and effect-owner hold/delete fencing. | V13 and RO cases; safe offboarding and financial closeout. |
| S-U06 | Contracts / Products | [Renewal terms](features/04-renewal-retention.md): explicit bound-term precedence, frozen evaluated deadlines and next-renewal decisions. | V14 and RO cases; contract-driven renewal and retention. |
| S-U07 | Financial admission owner (a `financial_admission` plugin instance, SUB-D-74) / Orders / Payments / Billing | [Financial admission](DESIGN.md#3311-financial-admission-integration-contract): retained payment or future postpay coverage, explicit dispositions, capture and recovery fences. | FA-C01–FA-C12 and V18; financially guarded operations without duplicate collection or live reassessment of an accepted purchase. |
| S-U08 | Pricing / Rating | [Rating contract](DESIGN.md#rating-result-contract): exact accepted bindings, concurrent lots, one-time occurrence/basis, supported minimum-fee scopes and complete replacements. Current Rating documents still describe a different continuation and one-time valuation; the owner alignment is tracked in the [Subscriptions proposal for the Rating contract](https://github.com/constructorfabric/gears-rust/pull/5390#issuecomment-6360750023). | Rating acceptance cases; supported recurring, usage, one-time and quantity-change valuation. |
| S-U10 | Orders / Products / Pricing / financial owner | [Occupancy contract](DESIGN.md#accepted-acquisition-evidence): versioned scope/limit, protected claims and atomic conversion/release. | V03; protected acquisition and supported key changes. |
| S-U11 | Product / SRE / consumers | [Measured recovery and workload evidence](features/09-consumer-contracts.md#39-replace-attribution-without-erasing-financial-history) against PRD budgets. | V16; production readiness. |
| S-U12 | Pricing / product policy owners | [Trial terms](DESIGN.md#336-trial-terms-and-provider-evidence): immutable variant, duration, conversion lead time (SUB-D-65), exhaustion action for credit trials (SUB-D-66), applicability, initial eligibility and accepted paid target. | TP cases; free-period and credit trials. |
| S-U13 | Billing / Rating / Ledger | [Financial replacements](DESIGN.md#financial-consumer-boundary): exact seals, invoice-version fencing, advances, final rounding and recoverable corrections, including historical unresolved intervals that survive cancellation. | Joint seal, replacement and financial-recovery cases; invoicing and closeout. |
| S-U15 | Billing / Rating / Pricing / Ledger | [Trial coverage](features/06-trials.md#credit-preparation-and-recovery): one reserved grant effective at activation, original expiry, allocation and correction. | TP cases; credit trials. Free-period trials do not require a promotional-grant provider. |
| S-U16 | Pricing / Orders Changes | [Change-delta acceptance](features/03-changes.md#apply-one-atomic-changecomposition): a delta accepted per change order/version/line against the target subscription and expected revision, bindings pinned at submit, frozen instant at the applied commit, hold window and expiry, and reuse after a definitively unapplied attempt with immutable Pricing revalidation when the expected revision changes (SUB-D-61). Revalidation binds the acceptance ID/digest to the new revision without repricing or extending expiry; absent or refused evidence blocks the retry. | V06 and AC 60; order-originated increases and additions. |
| S-U17 | Settings Service / governance owner | [Policy set](DESIGN.md#42-gts-identifiers): the `policy_set` settings category with platform and seller-tenant scopes, resolved value with version and provenance, authorized change, and governance minima as floors (SUB-D-72). | V14 and RO cases; renewal, grace, retention and preparation deadlines evaluated from the resolved set. |
| S-U18 | Serverless runtime / task engine | [Lifecycle hooks](DESIGN.md#42-gts-identifiers): execution of `lifecycle_hook` plugin instances at `before_apply` (allow or typed veto within the operation budget, fail closed) and `after_applied` (at-least-once from the outbox), with the hook snapshot schema and the registry (SUB-D-73). | Hook veto, timeout and at-least-once delivery cases; until delivered the hook stage is a no-op. |

Preparation policy is snapshotted on each attempt. Applicable S-U01/S-U04/S-U07/S-U13 mappings must prove bounded preparation, deadline/activation arbitration and platform accounting for authorized pre-activation use. Unknown effects remain recoverable; neither a timeout nor delayed publication releases held preparation or spends trial credit. See [readiness](features/05-entitlements.md#initial-activation-readiness-contract) and [trial coverage](features/06-trials.md).

## 3. Priorities

All incoming requirements above are `p1`. Enable only profiles within [PRD scope](PRD.md#11-purpose) whose applicable provider contracts pass. Deferred capabilities create no current delivery dependency.

## 4. Traceability

- [PRD](PRD.md): `Covers` maps incoming requirement IDs to product requirements.
- [DESIGN](DESIGN.md), [features and acceptance cases](DECOMPOSITION.md): architecture, implementation allocation and V scenarios.
- [DECISIONS](DECISIONS.md): commercial and ownership decisions.
