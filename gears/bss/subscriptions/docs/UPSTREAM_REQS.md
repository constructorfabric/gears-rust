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

Subscriptions MUST expose separate draft creation and activation, bind operations to order/version/line/attempt identity, and distinguish acceptance from application. Replay, withdrawal, status lookup and uncertain-create settlement MUST preserve one acquisition and its financial obligations. A successor attempt requires fenced predecessor settlement.

- **Consumer:** Orders Lifecycle and Workflow.
- **Contract:** [Activation](DESIGN.md#332-pricing-acceptance-and-accepted-time), [foundation](features/01-foundation.md).
- **Acceptance:** V01–V02 and AP-C01–AP-C12 cover lost replies, cancellation, partial order fulfillment and recovery.

#### Occupancy and atomic entry

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-occupancy`

Subscriptions MUST expose the authoritative scope key, limit and policy version with separate active, retained-suspended and order-reserved counts. Admission and claim conversion MUST be atomic. Confirmation reserves before capture; activation or unchanged-key resume consumes its own right once. Paid or uncertain claims survive technical expiry. Lowering the limit does not evict protected claims.

- **Consumer:** Orders Lifecycle and Changes.
- **Contract:** [Foundation](features/01-foundation.md), [activation](DESIGN.md#332-pricing-acceptance-and-accepted-time).
- **Acceptance:** V03 covers concurrent admission, suspension, reservation conversion, failed key movement and fenced release.

#### Atomic composition change

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-upreq-composite-change`

Subscriptions MUST apply an admitted multi-item change to one subscription in one transaction against the expected revision. Validate the resulting bounds, eligibility and occupancy together. Increases and additions require an active subscription; suspended purchases refuse at submission and commit. Unsupported dates or items refuse the whole request before effects.

- **Consumer:** Orders Changes.
- **Contract:** [Composition changes](features/03-changes.md).
- **Acceptance:** V06 covers suspension races, stale revisions, invalid bounds and atomic refusal.

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
| S-U01 | Orders / Workflow / Pricing / Products | [Protected acquisition](DESIGN.md#332-pricing-acceptance-and-accepted-time): durable accepted future terms, capture/cancel ordering, attempt association and recovery. | AP-C01–AP-C12; acquisition, initial activation and supported successor attempts. |
| S-U02 | Inventory / resource evidence provider | [Historical inventory](features/09-consumer-contracts.md#39-replace-attribution-without-erasing-financial-history), including deleted resources, lifecycle barriers and typed gaps. | V09; attribution and sealed ownership scopes. |
| S-U03 | Usage emitter / Products / Rating | [Usage coverage](features/09-consumer-contracts.md#39-replace-attribution-without-erasing-financial-history), independent of ownership and collector progress. | V07 and V09; final usage valuation and invoicing. |
| S-U04 | Entitlements | [Posture and held readiness](features/05-entitlements.md#initial-activation-readiness-contract), exact terminal resolution, timeout and unknown-outcome recovery. Entitlements owns adapter conformance. | V11 and V19; entitlement-dependent activation and lifecycle application. |
| S-U05 | Entitlements / Products / Billing / Rating | [Offboarding](features/04-renewal-retention.md): confirmed cutoff, actual stop, retention and effect-owner hold/delete fencing. | V13 and RO cases; safe offboarding and financial closeout. |
| S-U06 | Contracts / Products | [Renewal terms](features/04-renewal-retention.md): explicit bound-term precedence, frozen evaluated deadlines and next-renewal decisions. | V14 and RO cases; contract-driven renewal and retention. |
| S-U07 | Financial admission owner / Orders / Payments / Billing | [Financial admission](DESIGN.md#3311-financial-admission-integration-contract): retained payment or future postpay coverage, explicit dispositions, capture and recovery fences. | FA-C01–FA-C12 and V18; financially guarded operations without duplicate collection or live reassessment of an accepted purchase. |
| S-U08 | Pricing / Rating | [Rating contract](DESIGN.md#rating-result-contract): exact accepted bindings, concurrent lots, one-time occurrence/basis, supported minimum-fee scopes and complete replacements. | Rating acceptance cases; supported recurring, usage, one-time and quantity-change valuation. |
| S-U10 | Orders / Products / Pricing / financial owner | [Occupancy contract](DESIGN.md#accepted-acquisition-evidence): versioned scope/limit, protected claims and atomic conversion/release. | V03; protected acquisition and supported key changes. |
| S-U11 | Product / SRE / consumers | [Measured recovery and workload evidence](features/09-consumer-contracts.md#39-replace-attribution-without-erasing-financial-history) against PRD budgets. | V16; production readiness. |
| S-U12 | Pricing / product policy owners | [Trial terms](DESIGN.md#336-trial-terms-and-provider-evidence): immutable variant, duration, applicability, initial eligibility and accepted paid target. | TP cases; free-period and credit trials. |
| S-U13 | Billing / Rating / Ledger | [Financial replacements](DESIGN.md#financial-consumer-boundary): exact seals, invoice-version fencing, advances, final rounding and recoverable corrections. | Joint seal, replacement and financial-recovery cases; invoicing and closeout. |
| S-U15 | Billing / Rating / Pricing / Ledger | [Trial coverage](features/06-trials.md#credit-preparation-and-recovery): one reserved grant effective at activation, original expiry, allocation and correction. | TP cases; credit trials. Free-period trials do not require a promotional-grant provider. |

Preparation policy is snapshotted on each attempt. Applicable S-U01/S-U04/S-U07/S-U13 mappings must prove bounded preparation, deadline/activation arbitration and platform accounting for authorized pre-activation use. Unknown effects remain recoverable; neither a timeout nor delayed publication releases held preparation or spends trial credit. See [readiness](features/05-entitlements.md#initial-activation-readiness-contract) and [trial coverage](features/06-trials.md).

## 3. Priorities

All incoming requirements above are `p1`. Enable only profiles within [PRD scope](PRD.md#11-purpose) whose applicable provider contracts pass. Deferred capabilities create no current delivery dependency.

## 4. Traceability

- [PRD](PRD.md): `Covers` maps incoming requirement IDs to product requirements.
- [DESIGN](DESIGN.md), [features and acceptance cases](DECOMPOSITION.md): architecture, implementation allocation and V scenarios.
- [DECISIONS](DECISIONS.md): commercial and ownership decisions.
