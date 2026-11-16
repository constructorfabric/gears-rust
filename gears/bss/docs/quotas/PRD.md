---
description: "Product requirements for quota catalogs, purchases and application through the BSS adapter contract."
---

# PRD: Commercial Quotas


<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
  - [4.3 Future scope](#43-future-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Catalog and plans](#51-catalog-and-plans)
  - [5.2 Purchase and attribution](#52-purchase-and-attribution)
  - [5.3 Delivery and enforcement](#53-delivery-and-enforcement)
  - [5.4 Changes and end of service](#54-changes-and-end-of-service)
  - [5.5 Visibility and recovery](#55-visibility-and-recovery)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Capability-specific requirements](#61-capability-specific-requirements)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [8.1 Buy and apply a tenant package](#81-buy-and-apply-a-tenant-package)
  - [8.2 Change quantity and recover a partial delivery](#82-change-quantity-and-recover-a-partial-delivery)
  - [8.3 Restrict and end service](#83-restrict-and-end-service)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

**Status: proposed.** Allocation quotas are the first profile. These requirements describe the target behavior. The [qualification gates](DESIGN.md#42-integration-qualification-gates) define what must be proven before use. Example amounts are not platform defaults.
## 1. Overview

### 1.1 Purpose

Define how provider capabilities become plan offers, accepted purchases and tenant limits applied through an adapter. Customers and operators must be able to trace each requested limit to its purchase and see whether it was applied.

This PRD defines commercial outcomes and business acceptance criteria. [DESIGN](DESIGN.md) specifies architecture, owner contracts and the complete Pricing-to-adapter path. [DECOMPOSITION](DECOMPOSITION.md) maps these requirements to five feature specifications: [catalog](features/0001-catalog.md), [plan rules](features/0002-plan-rules.md), [purchase](features/0003-purchase.md), [application](features/0004-application.md) and [qualification](features/0005-qualification.md). JSON contract examples remain in `schemas/`.

### 1.2 Background / Problem Statement

A tenant buys a service plan with limits such as 10 vCPU and 20 GB RAM. BSS preserves the purchased terms, assigns the allowance to the tenant that owns the resources, and applies it through the selected adapter. OSS distributes this allowance across resources and enforces it when users create or change them.

An allocation limit does not mean usage is prepaid or included. The accepted plan defines package charges, usage charges and included usage separately.

Without a common contract, a technical limit could become a sellable right without approval. Plan edits could change old purchases, and separate quota records could duplicate an allowance. A saved limit could also appear ready even though the provider cannot deliver it.

### 1.3 Goals (Business Outcomes)

- Every purchased allowance traces to immutable accepted terms and an authorized resource owner.
- A supported purchase becomes usable once its quota is ready. Resource readiness is also required if the purchase includes deployment.
- Duplicate delivery, retry, activation and renewal create no extra allowance or restore superseded rights.
- BSS exposes purchased limits, their application status and adapter-reported fulfillment discrepancies. OSS owns operational quota state and usage reads.
- Unsupported resource and profile combinations are rejected before sale.

### 1.4 Glossary

| Term | Definition |
| --- | --- |
| Metric descriptor | Versioned technical meaning: resource, unit, subject scope and supported enforcement behavior |
| Quota rule | Commercial limit semantics attached to a plan revision, such as 10 vCPU per plan unit |
| Binding | Selection of a rule for a plan item, dimension and subject-binding policy |
| Accepted snapshot | Immutable record of the selected rules, quantity and supporting evidence |
| Allocation quota | Limit on resource capacity managed by OSS. The limit does not determine usage charges. |
| Plan quota / purchased limit | Allocation ceiling authorized by accepted plan terms, including a usage-priced plan. Not proof of prepaid or included usage |
| Included usage | Usage covered by the accepted charging terms, measured under their billing meter and period. Distinct from an allocation ceiling |
| Consumption quota | Limit on cumulative consumption in a defined window. Separate future profile |
| Purchased cap | Amount promised by accepted terms for the relevant interval |
| Effective restriction | Actual independent provider/operator constraint, which can be lower than the purchased cap |
| Remaining | OSS-authoritative available amount after occupied/reserved capacity and applicable restrictions. Read through OSS contracts |
| Resource tenant | Tenant that owns the resource scope. May differ from seller, payer and the acting user's home tenant |
| Provider scope | Logical boundary in which a provider enforces a quota and accounts for allocations, identified through an authorized adapter binding |
| Assignment | Stable fulfillment identity for one acquisition, logical binding and preparation generation, with its source-owned quota members |
| Quota application | One adapter operation that establishes the complete desired quota set across all required OSS systems |
| Profile | Versioned, tested set of supported metrics, scopes, customer-facing conditions, application and recovery guarantees |

## 2. Actors

### 2.1 Human Actors

#### Product engineer / catalog administrator

**ID**: `cpt-cf-bss-quotas-actor-catalog-owner`

- **Role**: Define resource meaning and approve provider mappings.
- **Needs**: Publish reviewed quota definitions and supported adapter capabilities.

#### Commercial plan author

**ID**: `cpt-cf-bss-quotas-actor-seller`

- **Role**: Package and price explicit limits without changing technical meanings.
- **Needs**: Configure understandable quota limits and separate charging terms on plans.

#### Authorized tenant purchaser

**ID**: `cpt-cf-bss-quotas-actor-buyer`

- **Role**: Buy and change an allowance and understand delivery status.
- **Needs**: Know the accepted limits, charging terms and fulfillment outcome.

#### Platform/provider operator

**ID**: `cpt-cf-bss-quotas-actor-operator`

- **Role**: Inspect restrictions, delivery gaps and operations that need recovery.
- **Needs**: Recover incomplete delivery using evidence tied to the operation and authorized scope.

### 2.2 System Actors

#### Products, Pricing, Orders, Subscriptions

**ID**: `cpt-cf-bss-quotas-actor-commerce`

- **Role**: Maintain catalog, accepted evidence and commercial lifecycle.

#### Entitlements and selected adapters

**ID**: `cpt-cf-bss-quotas-actor-fulfillment`

- **Role**: Apply and reconcile committed rights.

#### OSS quota and resource services

**ID**: `cpt-cf-bss-quotas-actor-enforcer`

- **Role**: Enforce applied limits and manage resource allocation and usage. The adapter reports quota application outcomes to BSS.

## 3. Operational Concept & Environment

The platform follows the [ToolKit architecture](../../../../docs/toolkit_unified_system/README.md) and [project guidelines](../../../../guidelines/README.md). This specification covers BSS responsibilities and its adapter contracts.

The named contract extensions describe required target capabilities. [DESIGN §4.2](DESIGN.md#42-integration-qualification-gates) defines their qualification requirements.

Several OSS systems can enforce the same allowance. The selected adapter coordinates them and returns one application result to BSS. The adapter confirms that OSS enforces the allowance in the agreed scope. Buying 10 vCPU grants quota permission, not a reservation of ten physical CPUs.

## 4. Scope

### 4.1 In Scope

- Resource onboarding, plan authoring and versioning.
- Acceptance, purchase and assignment to the resource-owning tenant.
- Quota application across OSS and confirmation of readiness.
- Compatible quantity changes, renewal, suspension, resume, expiry and cancellation.
- Commercial limits, application status, repair and profile qualification.

### 4.2 Out of Scope

The first release excludes:

- Shared pools across acquisitions or providers, redistribution of tenant budgets to child tenants and per-user budget partitions.
- Arbitrary formula languages, unlimited quotas and allocation above a hard cap.
- Numerical decreases of purchased quota limits, quantity decreases and base-plan changes.
- Automatic adoption of legacy subscriptions and resource migration between providers.
- General consumption and rate-limit profiles.

These need further commercial and enforcement contracts. Plan publication cannot enable them implicitly. Suspension and revocation still restrict access. Independent provider restrictions may lower effective capacity without changing purchased limits.

Entitlements and Subscriptions define activation, combined readiness, activation deadlines, cancellation decisions and preparation cleanup. This package defines how quotas contribute to those contracts.

Resource lifecycle and adapter designs own allocation, admission, reservations, counters, release, stop, resize, delete, retention and cleanup. Their internal contracts and tests are outside this package. BSS does not receive resource allocation or release events.

Each adapter design owns its connections, internal OSS APIs, credentials and mappings. OSS owns usage, reservations, effective limits, remaining capacity and admission decisions, including their storage, calculation and read APIs. A portal can combine these values with BSS limits and application status. Entitlements does not collect, store or calculate operational usage.

Quota rules are separate from accepted charging terms, so the contract supports package, usage-priced and mixed plans. Financial and usage owners keep rating, usage ingestion, included-usage accounting and tariffs. Existing owners also keep monetary credit, charging, taxes, refunds, named license grants and retention policy. Quota state provides evidence and follows authorized posture. It does not decide these policies.

### 4.3 Future scope

Future work includes before/after change previews with reasons, an explanation and history API/UI, and combined add-ons, promotions, negotiated overrides or multiple subscriptions. The broader profiles in §4.2 are also deferred.

V1 still requires accepted-terms previews, immutable history and audit records, application status and discrepancy reasons. DESIGN §4.1 defines these boundaries. Deferred features do not block the first profile.

## 5. Functional Requirements

Each entry defines a separately verifiable outcome. Protocol fields, schemas and algorithms belong to DESIGN and the linked feature specifications.

### 5.1 Catalog and plans

#### Reviewed resource onboarding

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-catalog`

Products MUST stage authorized discovery and manual input as catalog candidates for review. This creates no price, purchased right or live tenant limit.

#### Registered resource semantics

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-catalog-gts`

Quota definitions MUST use registered GTS metric and unit identities with validated versions, resource and subject types, technical bounds and supported operations.

#### Understandable quota selection

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-catalog-presentation`

Each published quota definition MUST provide a reviewed localized name and description, an English fallback, its unit and the applicable accounting conditions for the selected profile.

#### Equivalent approval paths

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-catalog-approval`

Manual and discovered definitions MUST pass the same identity, semantic validation and catalog approval rules before use in a plan.

#### Resource-limit classification

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-catalog-classification`

The catalog MUST distinguish numerical resource limits from monetary controls, feature permissions and retention policies.

#### Historical meaning

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-catalog-history`

Published definition versions MUST stay readable with their original meaning for accepted purchases. Compatible registry changes cannot reinterpret them.

#### Explicit accounting and scope

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-catalog-policies`

Each qualified definition MUST identify immutable capacity and scope semantics. These state what the limit covers, including relevant stopped-resource conditions and aggregation dimensions. BSS retains this meaning for plan authoring and purchase history; OSS defines its accounting mechanisms.

#### Schema compatibility

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-catalog-compatibility`

Catalog schema changes MUST follow the platform GTS compatibility policy. They must preserve historical evidence and use a new major version for breaking changes.

#### Revision-owned quota rules

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-plan-rules`

A plan revision MUST express each allowance as a versioned rule for its service or resource binding. Different plans for the same product can offer different limits.

#### Applicable quota catalog

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-plan-catalog`

Plan authors MUST be able to browse approved definitions for the selected product version and read exact historical versions. Each shows its description, units, conditions and current sale availability.

#### Exact commercial amounts

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-plan-amount`

Plan validation MUST refuse unknown metrics, unsupported scopes, negative or unrepresentable amounts, inexact unit conversions and incompatible quota combinations before publication.

#### Absent and zero limits

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-plan-absence`

The plan MUST distinguish an absent quota declaration from an explicit zero limit and from unbounded capacity.

#### Explicit semantic changes

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-plan-revision`

Changing quota meaning or capacity MUST require a new plan revision and acceptance even when the price is unchanged.

#### Independent charging terms

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-charging-separation`

Plans MUST show and retain package, usage-priced or mixed charging terms separately from the allocation ceiling for the same service binding. The ceiling implies no prepayment, billing inclusion or payment schedule.

#### Complete draft editing

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-plan-edit`

Authorized plan authors MUST be able to read, add, edit and remove quota rules as one complete draft change. If any part is invalid, the saved content must stay unchanged.

#### Safe concurrent authoring

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-plan-edit-conflict`

Plan editing MUST refuse stale or conflicting changes while recovering an identical retry without applying it twice.

#### Quota-aware plan governance

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-plan-governance`

Plan checks, copy/clone, approval, publication and purchase acceptance MUST include quota content and preserve published and accepted terms. Charging objects remain separate from quota rules.

#### Committed quantity basis

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-quantity`

Quota derivation MUST use Subscriptions' current committed effective quantity. Each per-plan-unit rule applies once per unit. Each fixed rule applies once per logical binding.

#### Immutable original quantities

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-quantity-provenance`

The original purchase and each compatible addition MUST remain traceable. The original receipt quantity must not change or replace the current effective quantity.

#### No duplicate allowance

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-quantity-no-duplication`

Duplicate selections, purchase retries and billing-meter quantities MUST NOT increase the allowance beyond its applicable rules and committed plan quantity.

#### Compatible additions only

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-quantity-compatibility`

Purchase and change admission MUST refuse unsupported independent component quantities, overlapping commercial sources and incompatible applicability, windows or terms.

#### Safe increase continuity

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-quantity-increase`

A compatible committed increase MUST update the existing assignment without creating another allowance. The previous safe allowance must remain until the complete increase is applied.

#### Scope continuity

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-scope-continuity`

The selected scope dimensions MUST keep the same meaning from plan to applied assignment. The full allowance must not be copied into independent regions or resource pools.

#### Protected ownership before commitment

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-purchase-exclusivity`

Purchase admission MUST exclude conflicting acquisitions across payers and products on the same authoritative quota ownership domain before commercial commitment or capture authorization.

#### Recoverable ownership protection

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-claim-retention`

An unresolved domain or ownership claim MUST block commitment. An existing claim MUST stay protected until the responsible owner confirms closure and safe reuse. BSS must not derive that confirmation from resource counters or release events.

### 5.2 Purchase and attribution

#### Frozen purchase evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-acceptance`

Acceptance MUST preserve the selected price and quota terms together, including applicable versions, original quantity, mappings, profile, validity and seller, payer and resource-owner identities.

#### Stable purchase replay

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-purchase-replay`

Repeating a purchase MUST recover its original accepted evidence. Changed content under the same purchase identity must be refused.

#### No live-term substitution

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-accepted-history`

Missing historical purchase evidence MUST block fulfillment without substituting a current plan or unrestricted access.

#### Authorized resource ownership

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-attribution`

The allowance MUST belong to the verified resource owner. It must not default to the acting user, purchaser, seller or payer. Those roles alone do not authorize another resource scope.

#### Shared tenant allowance

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-shared-tenant`

BSS MUST assign the purchased allowance to the resource-owning tenant scope once. It must not create a separate copy for each user.

#### Retained scope attribution

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-scope-binding`

The application MUST retain the verified subject and provider-scope mapping used for the purchase and its recovery.

#### Committed authority to fulfill

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-committed-authority`

Only committed, authorized commercial posture MUST authorize quota fulfillment. A draft or acceptance receipt alone grants no usable quota.

#### Complete quota readiness

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-readiness`

Quota readiness MUST require adapter confirmation that the complete desired set is applied and usable for the authorized scope and current requested revision.

#### Limits without deployment

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-readiness-no-resources`

A purchase of resource limits only MUST become quota-ready without creating resources. Resource readiness and activation remain separate owner decisions.

### 5.3 Delivery and enforcement

#### One complete application

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-application-state`

Entitlements MUST submit the complete desired set through one selected adapter operation. Success confirms the complete change across all required OSS systems.

#### Truthful application outcomes

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-application-outcomes`

Application reporting MUST distinguish complete success, incomplete work, uncertain effects and definitive refusal. A refusal cannot leave unresolved effects or recovery obligations.

#### Recoverable incomplete delivery

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-application-recovery`

Incomplete or uncertain application MUST remain recoverable through the original adapter and operation identity.

#### One purchased allowance

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-application-copies`

Adapter application across several OSS systems MUST NOT multiply the purchased allowance.

#### Enforced aggregate scope

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-aggregate-scope`

An aggregate allowance MUST be offered only where OSS has proven that it enforces the total across the advertised scope.

### 5.4 Changes and end of service

#### Application continuity

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-lifecycle`

Supported increases, renewal, suspension and resume MUST preserve the continuing assignment and its application history. They must not create a second purchased allowance.

#### Confirmed access restriction

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-restriction-fallback`

The adapter MUST confirm that the requested suspension or revocation is enforced in the agreed scope. BSS cannot infer restriction from a saved zero limit.

#### Irreversible source closure

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-terminal-closure`

Cancellation or expiry MUST close the old acquisition against every later grant, increase or resume, including delayed work and newer requests.

#### Recovery after closure

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-closed-recovery`

Authorized reads and recovery of original quota application operations MUST remain possible after closure without restoring access.

#### Distinct replacement purchase

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-new-purchase-isolation`

A new purchase MUST use a distinct source. It must not inherit another purchase's allowance or unresolved application operations, or bypass retained scope ownership.

#### Exact service validity

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-validity`

An offer MUST enforce a start-inclusive and end-exclusive validity interval under its supported provider profile.

#### Financial decisions remain separate

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-financial-separation`

Billing cutoff, credit depletion and refunds MUST follow financial and lifecycle owner decisions. Quota application outcomes cannot decide those financial actions.

### 5.5 Visibility and recovery

#### Commercial fulfillment visibility

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-visibility`

Authorized BSS readers MUST see purchased and desired limits and their accepted-term provenance. They must also see application status tied to its revision, confirmation time and discrepancy evidence.

#### OSS operational reads

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-visibility-oss`

Operational used, reserved, remaining and effective-limit values MUST come from OSS. Entitlements must not collect, store or calculate an operational usage projection.

#### Independent restrictions remain visible

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-discrepancy`

A restriction that prevents full delivery MUST remain a fulfillment discrepancy without changing purchased terms. Ordinary resource consumption is not a discrepancy.

#### Explicit stale evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-visibility-freshness`

Missing or stale application evidence MUST remain explicit and cannot imply successful fulfillment or clear an unresolved discrepancy.

#### Complete input evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-complete-evidence`

Receivers MUST verify that quota evidence is complete before deriving limits or treating missing members as removal. Partial input cannot mean an empty set.

#### Recovery without extra rights

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-recovery`

Delivery recovery MUST survive duplicate events, process death, lost responses and reordered revisions. It must not duplicate grants or restore superseded rights.

#### Source-owned repair

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-recovery-authority`

Reconciliation MUST preserve commercial intent and repair only authorized source-owned records. Independent operator restrictions must be reported, not overwritten.

#### Accepted-order protection

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-accepted-ordering`

An authorized change that has been durably accepted MUST prevent older work from newly taking effect, even if the accepted change later fails. Accepted and applied states must stay distinct.

#### Stable refusal recovery

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-refusal-replay`

Replaying a definitively refused operation MUST return its original refusal. A corrected attempt must be distinct and must not create another purchase.

#### Retained unresolved effects

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-superseded-evidence`

Superseded changes MUST retain unresolved quota application operations and their evidence until the adapter resolves them.

#### Durable failure handoff

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-failure-handoff`

Definitive refusal or exhausted automatic retries MUST preserve the unfulfilled obligation and its reason in Entitlements' durable recovery or escalation process.

#### No invented outcome or remedy

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-failure-no-remedy`

Retry exhaustion MUST NOT invent a definitive application result, cancel the subscription or start a refund.

#### Qualified offers only

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-qualification`

A plan MUST be offered only under a qualified integration profile whose identity, units, scope, application, lifecycle, authority, recovery and readiness guarantees cover the complete quota set.

#### Owned qualification evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-qualification-evidence`

The adapter owner MUST supply conformance evidence for the integration version. Products retains it and uses it to control sale availability. Discovery declarations alone do not authorize sale.

#### Eligibility at each sale boundary

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-qualification-recheck`

Pricing MUST revalidate the complete required set against enabled qualification at publication and purchase or change acceptance. Known incompatibility must be refused before commitment or capture authorization.

#### Qualified installation and increase

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-qualification-changes`

Initial installation and capacity increase MUST be qualified separately. An initial-only profile cannot authorize an increase.

#### Withdrawal preserves accepted rights

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-qualification-withdrawal`

Withdrawn qualification MUST block new sales while preserving accepted obligations.

#### No implied hardware reservation

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-physical-capacity`

Successful quota validation MUST NOT imply physical stock or a hardware reservation. OSS owns physical capacity checks and reservations.

#### Accepted obligations during outage

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-fr-provider-outage`

Temporary failure of a qualified provider MUST preserve the accepted obligation as pending or uncertain fulfillment. It must not invent delivered service, cancel the purchase or create another one.

## 6. Non-Functional Requirements

### 6.1 Capability-specific requirements

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-nfr-isolation`

Writes and recovery reads MUST reject unauthorized tenant/source access on both local and remote SDK paths. Cross-tenant checks MUST neither change data nor expose protected state.

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-nfr-durability`

Every committed desired revision MUST retain durable delivery intent and a recoverable result or an explicit unresolved finding. In every fault-matrix case, retries MUST cause no extra grants or stale regrants. Terminal fences MUST last through all permitted retry periods.

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-nfr-operations`

Every enabled profile MUST define finite request timeouts, concurrency and queue limits, a retry budget, an observation freshness threshold and an escalation deadline. Exceeding a threshold MUST create a durable finding for action through Entitlements recovery/escalation. Overload MUST not turn an unknown outcome into success. Qualification MUST record measured behavior against these values.

### 6.2 NFR Exclusions

Availability and latency targets depend on the deployment and the selected adapter. Each profile needs deployment evidence for its delivery SLOs. This document sets no unsupported vendor performance targets. Project security, audit and runtime baselines still apply.

## 7. Public Library Interfaces

### 7.1 Public API Surface

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-interface-management`

**Type:** versioned owner SDKs and their authorized UI/REST interfaces.

**Stability:** proposed extensions.

Products owns catalog authoring and reads. Pricing owns rules and acceptance. Orders and Subscriptions own purchases and changes. Entitlements owns fulfillment status and reconciliation. OSS owns runtime counters.

Breaking changes require versioned contracts. Buyers get no separate generic API for writing caps.

### 7.2 External Integration Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-contract-provider`

**Catalog input:** Products reads capability snapshots through the selected discovery adapter SDK. Records use registered GTS metric/unit identities and typed capability schemas as specified in DESIGN §3.3.1. Discovery produces catalog candidates for review. It grants no tenant allowance.

**Application direction:** required from the quota application adapter selected by BSS.

**Protocol:** versioned platform SDK.

Entitlements sends authorized desired quotas. The adapter returns application status, effective-limit evidence and recoverable outcomes tied to the operation. It must preserve scope isolation, replay safety and ordering. Its design selects the receiving OSS component and downstream management APIs.

## 8. Use Cases

### 8.1 Buy and apply a tenant package

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-usecase-buy`

**Actors:** catalog owner, seller, buyer and fulfillment actors above.

**Precondition:** a verified provider scope and a qualified profile exist.

1. Products describes vCPU and RAM with supported units and tenant/provider mappings.
2. Seller publishes a plan with 10 vCPU and 20 GB per plan unit; buyer accepts quantity one for the resource tenant.
3. Orders and Subscriptions commit the purchase/preparation operation; Entitlements submits the complete quota set to the selected adapter.
4. The adapter confirms that the complete quota set is applied and enforced for the tenant scope. Entitlements reports fulfillment readiness; Subscriptions activates under its commercial guards without waiting for a VM to be created.
5. OSS handles resource requests within that allowance. BSS receives no per-resource allocation or release events.

**Alternative flows:** a refused application keeps its reason and the unfulfilled obligation. An uncertain application remains recoverable through the original adapter operation. Quota readiness does not guarantee physical capacity.

### 8.2 Change quantity and recover a partial delivery

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-usecase-change`

**Actor:** buyer.

**Precondition:** active quantity one with a confirmed 10-vCPU allowance.

An accepted compatible increase changes effective quantity to two and the desired cap to 20 vCPU under the same assignment. If OSS applies only part of the change, the increase stays pending. The previous safe allowance remains until the complete change is confirmed. A retry recovers the same increase.

Publishing a different plan coefficient cannot change this purchase. The first subscription profile refuses numerical cap decreases. Independent restrictions remain visible as fulfillment discrepancies.

### 8.3 Restrict and end service

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-usecase-end`

**Actors:** commerce, fulfillment, operator.

**Precondition:** an active purchase with an applied quota assignment.

Subscriptions commits a restriction or terminal posture. Entitlements sends it through the saved adapter binding and tracks the result. A delayed old grant cannot reopen the acquisition. Application history and recovery remain available after closure. OSS handles existing resources and their accounting without sending release events to BSS.

## 9. Acceptance Criteria

These business criteria verify §5–6. Their `Q-C` labels identify checks, not additional requirements. [DECOMPOSITION](DECOMPOSITION.md) assigns technical criteria to features. [DESIGN](DESIGN.md) defines the integration qualification gates.

Each enabled offer must meet the applicable criteria. It must trace from approved catalog terms through the accepted purchase to confirmed application. Unsupported offers are refused before commitment. A temporary delivery failure preserves the purchased obligation and shows the recovery state.

| Criterion | Business acceptance outcome |
| --- | --- |
| Q-C01 Accepted evidence | Purchased rules, units and original quantity remain those accepted by the customer after the catalog or plan changes. Recovery preserves that evidence. Missing history never becomes unlimited access. |
| Q-C03 Quantity basis | A per-plan-unit allowance is multiplied once by the accepted/effective plan quantity. A fixed allowance applies once. Selecting several items does not create independent component quantities or duplicate the same allowance. |
| Q-C04 Quota readiness | A limits-only purchase becomes quota-ready when the complete allowance is confirmed usable. It needs no VM creation. Partial or uncertain fulfillment is not readiness. A draft grants no access. Resource readiness and activation remain separate owner decisions. |
| Q-C05 Purchase replay | Repeating the same purchase recovers the same purchased quantity and accepted terms without another allowance, even through another transport attempt. A changed request cannot replace the original purchase silently. |
| Q-C06 Compatible increase | A second compatible plan unit changes a 10-vCPU-per-unit allowance from 10 to 20 vCPU under the same assignment. Fixed(10 vCPU) stays 10. The original purchase still records one unit. Both purchases remain traceable. Repetition adds no capacity. Unsupported combinations are refused. |
| Q-C07 Suspended purchase and race | A suspended service cannot accept or queue a new addition/increase for automatic execution. Suspension before purchase commitment prevents new quantity and fulfillment effects. Resume needs separate authorization. An already committed purchase stays in history but cannot override a newer restriction. |
| Q-C11 Tenant isolation | Purchased capacity belongs to the authorized resource owner. Being a seller, payer or acting user does not authorize another tenant's resources or disclose that tenant's quota state during recovery. |
| Q-C13 Expiry geometry | An offer's validity is start-inclusive and end-exclusive: access is not extended by a delayed expiry process. A provider that cannot honor that boundary cannot sell the timed profile. |
| Q-C14 Mixed grants | Unsupported combinations of periods, units, expiry or commercial terms are refused before purchase/change. The system does not silently sum limits, choose a maximum or extend validity. |
| Q-C20 Financial separation | Monetary minimums and exhausted trial credit do not directly change numerical quota amounts. Access restrictions follow separately authorized commercial posture and the provider's supported enforcement behavior. |
| Q-C22 Binding equivalence | Reusing a plan item identifier does not make changed quota terms equivalent. Combining purchased quantities requires compatible quota meaning and applicability, not only matching item names or IDs. |
| Q-C27 Purchased limit and fulfillment discrepancy | The customer keeps the purchased limit and sees a fulfillment discrepancy when a provider restriction prevents full delivery. Ordinary consumption neither changes the purchased limit nor creates a delivery gap. Using 40 GiB of a usable 100 GiB allowance is normal. An external ceiling of 60 GiB prevents delivery of the purchased 100 GiB. BSS needs no usage ledger. |
| Q-C39 Plan packaging | The same product can be offered with different limits in different plans. The accepted plan determines the cap. Per-unit rules scale once by quantity. Fixed rules apply once. Duplicate selections add no allowance. Unsupported CPU-only add-ons and voluntary decreases are refused. |
| Q-C44 Shared users | BSS applies one purchased allowance to the resource-owning tenant scope. It does not create separate copies for individual users. The adapter confirms the agreed aggregate scope. |
| Q-C49 Numerical decrease refused | A request to lower a purchased quota limit is refused before effects in v1, without querying resource occupancy. The purchased limit stays unchanged. Suspension and revocation use their separate access controls. |
| Q-C55 Quota and charging independence | Package, usage-priced and mixed offers can each have a 20-vCPU ceiling. Allocating 4 vCPU uses the same capacity in each. Accepted charging terms define included and chargeable usage separately. A purely usage-priced component incurs no usage charge when unused. Exhausting a billing inclusion neither raises nor bypasses the cap. Scaling still follows Fixed/PerPlanUnit. Quota application carries no price or paid flag. |

## 10. Dependencies

| Dependency | Required contribution | Criticality |
| --- | --- | --- |
| Products / Pricing | Versioned metric/rule/acceptance extensions and historical retention | p1 |
| Types-registry and discovery adapters | Registered GTS schemas/instances, authenticated capability snapshots and reference validation | p1 |
| Orders / Subscriptions | Exact quantity changes, committed posture and activation resolution | p1 |
| Entitlements | Desired state, adapter operation tracking, delivery, readiness and reconciliation. Owner design pending in [PR #5744](https://github.com/constructorfabric/gears-rust/pull/5744) | p1 |
| Quota application adapter | Consistent application, operation recovery and effective-limit evidence across OSS | p1 |
| Adapter and scope authority | Verified scope binding, customer-facing capacity semantics and confirmation that OSS enforces the applied limits | p1 |
| Authorization and platform infrastructure | Scoped SDK access, durable events/tasks, credentials and egress | p1 |

Entitlements decision and gate references in this package are pending owner-contract dependencies from [PR #5744](https://github.com/constructorfabric/gears-rust/pull/5744), not delivered capabilities.

## 11. Assumptions

The first profile has bounded allocations and one commercial source per resolved quota scope. The adapter coordinates any OSS systems that enforce the same allowance. Existing-service adoption remains outside the initial profile. OSS handles any resource state within an eligible scope. Deployment profiles must define exact units, clocks and operating bounds before publication.

## 12. Risks

| Risk | Impact | Required treatment |
| --- | --- | --- |
| Provider API lacks fencing or safe multi-metric behavior | Stale grant or partial entitlement | Block profile until an effect-boundary protocol is demonstrated |
| Provider identity scope differs from platform tenant | Independent scopes receive duplicated caps | Prove aggregate mapping or restrict the offer to one provider scope |
| Adapter reports configuration without enforcement | Purchased limits appear delivered but are not enforced | Require application confirmation for the agreed scope and expose fulfillment failures |
| Unqualified enforcement capability | Selling unsupported limits | Require SDK and deployed-provider conformance before enabling a profile |

## 13. Open Questions

The scope-resolution contract still needs agreement with adapter and resource-service owners. BSS requires a stable, authorized binding and safe replay. Each adapter design chooses the OSS components that provide them.

[DESIGN](DESIGN.md) defines the architecture. Qualification must verify supported adapter versions, units and scope, application outcomes, protection from stale writes, operating bounds and readiness. Each has a gate in [DESIGN §4.2](DESIGN.md#42-integration-qualification-gates). An unresolved capability blocks the affected offer.

## 14. Traceability

[DECOMPOSITION](DECOMPOSITION.md) assigns every functional and non-functional requirement to its feature specification. [DESIGN §5](DESIGN.md#5-traceability) maps the architecture to that decomposition. Business criteria are defined in [§9](#9-acceptance-criteria). Technical criteria keep their stable `Q-C` labels in the responsible feature specifications.
