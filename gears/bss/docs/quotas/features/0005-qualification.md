---
description: "Enable only profiles with evidence that the BSS adapter contract applies the promised limits and access posture."
---

# Feature: Adapter qualification


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Adapter qualification](#21-adapter-qualification)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Adapter qualification checks](#31-adapter-qualification-checks)
  - [3.2 Qualification and sale availability](#32-qualification-and-sale-availability)
  - [3.3 Commercial policy and scope conformance](#33-commercial-policy-and-scope-conformance)
  - [3.4 Scope binding](#34-scope-binding)
  - [3.5 Qualification evidence](#35-qualification-evidence)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Owner-contract conformance](#51-owner-contract-conformance)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-featstatus-qualification-implemented`
- [ ] `p1` - `cpt-cf-bss-quotas-feature-qualification`

## 1. Feature Context

### 1.1 Overview

Enable a profile only after the adapter owner proves that it can apply the advertised limits, scopes and access posture. Products stores this evidence and controls sale availability.

### 1.2 Purpose

This feature covers adapter contract qualification, commercial policy compatibility, scope selection and sale availability. Resource allocation, counters, reservations and release are OSS responsibilities. Their implementation and tests belong to the OSS and adapter designs.

**Requirements**:

- `cpt-cf-bss-quotas-fr-shared-tenant`
- `cpt-cf-bss-quotas-fr-aggregate-scope`
- `cpt-cf-bss-quotas-fr-restriction-fallback`
- `cpt-cf-bss-quotas-fr-visibility-oss`
- `cpt-cf-bss-quotas-fr-qualification`
- `cpt-cf-bss-quotas-fr-qualification-evidence`
- `cpt-cf-bss-quotas-fr-qualification-changes`
- `cpt-cf-bss-quotas-fr-qualification-withdrawal`
- `cpt-cf-bss-quotas-fr-physical-capacity`
- `cpt-cf-bss-quotas-nfr-isolation`
- `cpt-cf-bss-quotas-nfr-durability`
- `cpt-cf-bss-quotas-nfr-operations`
- `cpt-cf-bss-quotas-interface-management`
- `cpt-cf-bss-quotas-contract-provider`
- `cpt-cf-bss-quotas-usecase-buy`
- `cpt-cf-bss-quotas-usecase-change`
- `cpt-cf-bss-quotas-usecase-end`

**Principles**: `cpt-cf-bss-quotas-principle-single-authority`.

### 1.3 Actors

| Actor | Role in feature |
| --- | --- |
| `cpt-cf-bss-quotas-actor-operator` | Start or qualify an operation within the authorized scope |
| `cpt-cf-bss-quotas-actor-enforcer` | Confirm supported limits, scope and adapter contract behavior |

### 1.4 References

- **PRD**: [Product requirements](../PRD.md).
- **Design**: [Architecture and contracts](../DESIGN.md), §3.3.1.3, 3.3.6, 3.5 and 4.2–4.5.
- **Decomposition**: [Feature allocation](../DECOMPOSITION.md).
- **Dependencies**: [Quota application and recovery](0004-application.md).
- **Pending owner contracts**: [Entitlements #5744](https://github.com/constructorfabric/gears-rust/pull/5744). All `ENT-D-*` and `E-U*` references point to the pending contracts listed in DESIGN §1.2.

## 2. Actor Flows (CDSL)

### 2.1 Adapter qualification

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-flow-qualification`

**Actor**: `cpt-cf-bss-quotas-actor-operator`.

**Use cases**: `cpt-cf-bss-quotas-usecase-buy`.

**Success Scenarios**: A supported, authorized request produces a result tied to that request, as defined by the owner contract.

**Error Scenarios**: Invalid or unauthorized requests are refused without effects. Uncertain external outcomes keep their original identity for recovery.

**Steps**:

1. [ ] - `p1` - Identify the exact integration, configuration, metric, mapping, accounting and scope policy versions. - `inst-profile`
2. [ ] - `p1` - Name the adapter owner and the authorized catalog and application targets. - `inst-owners`
3. [ ] - `p1` - Record the selected OSS integration limitations and its adapter-design reference against the required guarantees. - `inst-gaps`
4. [ ] - `p1` - Verify initial application, supported changes, access posture and operation recovery through the adapter contract. - `inst-evidence`
5. [ ] - `p1` - Retain measured outcomes, failure evidence and limitations in the qualification record. - `inst-record`
6. [ ] - `p1` - Enable sales only when authoritative evidence closes every applicable gate. - `inst-enable`
7. [ ] - `p1` - Withdraw unavailable qualification for new sales while retaining accepted obligations for recovery. - `inst-withdraw`

## 3. Processes / Business Logic (CDSL)

### 3.1 Adapter qualification checks

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-algo-qualification`

**Input**: Authorized owner request and the pinned evidence defined in DESIGN §3.3.1.3, 3.3.6, 3.5 and 4.2–4.5.

**Output**: A result tied to the request, or an explicit refusal or unresolved outcome.

**Steps**:

1. [ ] - `p1` - Verify the adapter owner's authority and exact qualification record - `inst-authority`
2. [ ] - `p1` - Match contract, mapping, policy, scope and configuration versions to the advertised profile - `inst-match-profile`
3. [ ] - `p1` - Check evidence for the complete quota set and required application, access and recovery operations - `inst-contract-evidence`
4. [ ] - `p1` - **IF** evidence is missing, incompatible or withdrawn - `inst-invalid-evidence`
   1. [ ] - `p1` - **RETURN** the reason without enabling new publication or purchases - `inst-refuse-enable`
5. [ ] - `p1` - Save the immutable qualification record and audit the authorized availability decision - `inst-persist-qualification`
6. [ ] - `p1` - **RETURN** the qualification reference and current sale availability - `inst-result`

### 3.2 Qualification and sale availability

The adapter owner supplies evidence for an exact integration version. It includes the contract version, target/profile, metric/unit/mapping/accounting-policy/scope-policy digests, supported combinations and change directions, numeric limits and measured operating bounds. Declaring `operations.apply = true` or registering a GTS record does not prove qualification. The adapter owner verifies the OSS integration in its own design and tests. Products and Pricing use the contract qualification result without receiving runtime accounting evidence.

Products stores an immutable `CapabilityQualification` record. Authorized catalog operations control its availability for new plan publication and purchases. The record names the accepted evidence, responsible adapter owner and tested configuration.

Availability is separate from the evidence version. An authorized catalog owner may enable a matching qualified record or disable it with an audited reason. The adapter owner may withdraw its evidence. Withdrawn evidence cannot support new sales. Changes to adapter, contract or mapping semantics need matching qualification before enablement. Discovery cannot promote a candidate automatically. Qualification may use isolated test scopes before commercial enablement.

At publication and purchase/change acceptance, Pricing checks the complete quota set against Products' enabled qualification. Checks cover exact metric, unit, accounting and scope policy versions; subject/resource types and scope selections; converted values and steps; change direction; required lifecycle operations; and joint application constraints. Qualifying vCPU and RAM separately does not prove they work together in one plan.

Unsupported or unresolved validation returns a specific reason. It cannot silently omit a member or choose another adapter. Publication and acceptance save the qualification identity/version and availability evidence used for the decision. An old draft check does not prove current eligibility.

Disabling or withdrawing qualification blocks new publication and purchases that use it. It does not erase accepted purchases, change limits, revoke resources or decide refunds. Existing obligations keep their pinned terms and use the application/recovery contract if fulfillment becomes unavailable. OSS still owns physical capacity checks and reservations. Plan validation proves quota compatibility, not available or reserved hardware.

### 3.3 Commercial policy and scope conformance

**Commercial meaning.** The catalog describes what the quota limits, including any profile-specific conditions the buyer needs to understand. For example, whether stopped resources count can affect the offer. BSS retains the accepted policy reference and meaning. It does not map resource states to counters or consume allocation and release events.

**Where it counts.** Each `PlanQuotaRuleV1` supplies `scope_selection` with exactly the keys its scope policy requires. The editor selects values from the authorized resource/catalog owner's dictionary. Free-text labels are not valid IDs. These values enter the revision, selected-term and receipt digests.

For a regional/pool offer, `20 vCPU` is one shared limit for the resource owner in that exact region/pool. Rules on the same logical binding must agree on scope policy and dimension values. The mapping must prove that these dimensions resolve to the saved provider scope. Changing them is an unsupported migration, not a retry.

An empty dimension map is valid only if the policy requires no dimensions and qualification proves the advertised tenant-wide aggregate. It never grants a full limit in every region/pool. Multiple provider scopes are unsupported unless one qualified logical enforcement scope enforces the total. This adds no cross-provider pool or budget distribution. Definition, plan, receipt and adapter request must retain the same scope meaning, even when the adapter translates opaque IDs.

The adapter qualification confirms that OSS enforces the advertised aggregate limit for the selected scope. BSS does not specify the resource admission algorithm or inspect individual allocations. If the limit cannot be applied or enforced as promised, the adapter reports an application failure or fulfillment discrepancy.

OSS supplies operational usage and remaining capacity through its own read contracts. Entitlements does not collect counters, calculate remaining capacity or reconcile resource accounting.

### 3.4 Scope binding

The initial profile binds each assignment to one verified provider scope. Purchase replay or target resolution must recover the saved assignment and binding. It cannot create a second scope with another full allowance. An unresolved binding cannot authorize application.

A tenant with several independent scopes cannot receive its full purchased allowance in each one. That topology needs an explicit partitioning or aggregate-enforcement profile. A per-resource limit cannot replace a shared tenant limit without proof of equivalent aggregation.

Scope and metric mappings stay fixed for an in-flight operation and remain available for recovery. Scope migration, aggregation changes and adoption of existing resources are outside the initial profile. The adapter returns an authoritative eligibility result for the requested binding. BSS does not inspect inventory or reset counters to make a scope eligible.

The adapter design covers how OSS protects the applied limit across resource interfaces. This package requires the promised enforcement result without specifying runtime controls or a resource-level test plan. Quota permission does not reserve hardware.

### 3.5 Qualification evidence

An acceptance record includes owner approval/reference, producer and consumer versions, the exact provider release, metric and subject mapping digests, capability profile/version, enabled/refused scenarios, fixture/run references and unresolved limits. Each provider implementation and profile qualifies separately. Agreement on documentation or a CRUD signature does not close a gate.

Verification covers authorized and unauthorized calls, duplicates, concurrent operations, restart, lost responses, partial outcomes and reordered revisions. BSS checks use the versioned owner/adapter contracts. The adapter must confirm applied limits and access posture. A dispatch acknowledgment or a passing local state-store check is not enough. Resource-level tests belong to the OSS and adapter designs.

Qualification records identify producer/consumer and provider versions, profile/mapping/policy digests, observed outcomes, executed evidence references and limits. Unknown or unsupported capabilities keep the affected gate open. The relevant business criteria in PRD §9 and technical criteria in DESIGN §4.3 need evidence before the offer is enabled. This feature records no test results and adds no separate activation, billing or resource-cleanup test plan.

## 4. States (CDSL)

This feature uses the states defined in DESIGN. It adds no lifecycle state machine. Only the responsible owner contracts change availability, draft/accepted status and commercial posture.

## 5. Definitions of Done

### 5.1 Owner-contract conformance

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-dod-qualification`

The responsible owners must implement this behavior and keep passing evidence for every criterion below. `DOCS-ONLY` traceability uses FEATURE checkboxes and evidence references to track implementation. It does not claim code markers or completed test runs.

**Implements**: `cpt-cf-bss-quotas-flow-qualification`, `cpt-cf-bss-quotas-algo-qualification`.

**Constraints**: `cpt-cf-bss-quotas-constraint-qualified-profiles`.

**Touches**: CapabilityQualification and adapter conformance boundaries (DESIGN §3.3.1, §3.3.6 and §4.2). State retention follows `cpt-cf-bss-quotas-db-owner-state` and the evidence entities in `cpt-cf-bss-quotas-entity-evidence-chain`.

Follow the shared security, error, storage, task-runtime and API rules in DESIGN §2.2 and §3.3.7. This feature adds no exceptions. Check scope on reads, writes, replay and recovery. Test concurrent calls, foreign scopes, response loss and restart wherever the flow has external effects. Owner unit tests cover pure validation and derivation. Contract and integration tests cover persistence, permissions and actual adapter effects. This change defines no new compile-time feature flag.

Rollback must keep accepted history, durable identities and unknown effects. Disabling new sales cannot erase existing obligations. Observability and recovery follow DESIGN §3.8 and Feature 4. The owner implementation supplies test code and deployment changes.

## 6. Acceptance Criteria

These criteria have stable verification IDs. Implementation is complete only when the required outcomes are proven. These are criteria, not test results. Business criteria remain in [PRD §9](../PRD.md#9-acceptance-criteria).

| Contract criterion | Relevant condition | Required guarantee |
| --- | --- | --- |
| Q-C18 Runtime capability | Missing required provider primitive versus temporary unavailability of a qualified provider | Missing capability blocks profile enablement. A temporary outage leaves delivery pending/unknown under the accepted acquisition contract. Do not invent an applied result or bypass the contract with direct DB changes. |
| Q-C21 Metric drift | A configured quota metric loses its enforced mapping or becomes unavailable | Distinguish stored configuration from working enforcement. Failed updates keep the purchased obligation and expose the need for owner remediation. Do not silently substitute a metric or report false readiness. |
| Q-C45 Provider scope fan-out | One tenant associated with multiple independent provider scopes with similar defaults | Do not copy the full purchased cap into both scopes. Refuse an unsupported aggregate profile. A provider hierarchy does not itself prove a shared budget. |
| Q-C48 Authorization | Seller/payer authority without foreign-resource ownership, or a service credential used to read another namespace | Reject mutations and protected-state reads without target-owner and delegated source authority. Check both on every operation. |
| Q-C50 OSS accounting ownership | BSS limits and application status are read alongside OSS usage | BSS exposes purchased limits and application state without collecting or storing operational usage. OSS supplies usage and remaining capacity through its own read contract. The adapter reports discrepancies that prevent the purchased limits from being applied or enforced. |
| Q-C54 Qualification, availability and overload | Unqualified declarations, individually qualified but incompatible metrics, adapter changes, qualification withdrawal after publication, and qualified-provider outage | Products enables only profiles with matching adapter-owner qualification. Pricing refuses unsupported complete sets and rechecks eligibility at acceptance. Old publication evidence cannot authorize a new sale. Disabled/withdrawn qualification blocks new sales but keeps accepted terms and obligations. Tests may run in isolated scopes before sales are enabled. OSS checks physical capacity. Outages and exhausted retries keep the obligation, original identities, latest outcome and reason. A durable handoff to Entitlements recovery survives restart. Definitive adapter refusal also leaves the obligation unresolved. Handoff or acknowledgment cannot mark the operation applied, cancel a purchase or refund money. Fallback is not success. |
| Q-C63 Accounting policy | Same-named metrics have different commercial accounting conditions, such as whether stopped resources count | Keep the exact policy reference and digest in the plan, receipt and assignment. Matching descriptions or units cannot substitute another policy. Runtime accounting is outside the BSS contract. |
| Q-C64 Scope continuity | A regional/pool allowance with changed or missing dimensions, scope aliases, or a full-cap attempt in another scope | Keep canonical authorized dimension values from plan through receipt to adapter binding. Refuse wrong, missing or changed dimensions. Resolve aliases through their authority instead of creating another domain. Tenant-wide offers need qualified shared aggregation. No silent wildcard, cross-region copy or implicit budget partition. |

The qualification record references the adapter design and its evidence for the required application, scope, access and recovery contract. Internal OSS accounting, resource operations and connections remain within the [adapter integration design boundary](../DESIGN.md#45-adapter-integration-design-boundary). Missing contract evidence keeps the affected gates open.
