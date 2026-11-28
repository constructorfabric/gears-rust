---
description: "Preserve accepted quota evidence through purchase and exclude conflicting owners of the same quota domain."
---

# Feature: Purchase attribution and quota exclusivity


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Purchase attribution and quota exclusivity](#21-purchase-attribution-and-quota-exclusivity)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Purchase attribution and quota exclusivity checks](#31-purchase-attribution-and-quota-exclusivity-checks)
  - [3.2 Accepted purchase and effective quantity](#32-accepted-purchase-and-effective-quantity)
  - [3.3 Protected purchase admission](#33-protected-purchase-admission)
  - [3.4 Change and distribution boundaries](#34-change-and-distribution-boundaries)
  - [3.5 Identity continuity](#35-identity-continuity)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Owner-contract conformance](#51-owner-contract-conformance)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-featstatus-purchase-implemented`
- [ ] `p1` - `cpt-cf-bss-quotas-feature-purchase`

## 1. Feature Context

### 1.1 Overview

Preserve accepted quota evidence through purchase and exclude conflicting owners of the same quota domain. Owners: Orders, Subscriptions and Entitlements.

### 1.2 Purpose

This feature covers accepted receipts, protected domain admission, committed effective quantity and the handoff of provenance. It does not add an activation or payment workflow, create resource scopes or reserve runtime quota.

**Requirements**:

- `cpt-cf-bss-quotas-fr-quantity`
- `cpt-cf-bss-quotas-fr-quantity-provenance`
- `cpt-cf-bss-quotas-fr-quantity-no-duplication`
- `cpt-cf-bss-quotas-fr-quantity-compatibility`
- `cpt-cf-bss-quotas-fr-scope-continuity`
- `cpt-cf-bss-quotas-fr-purchase-exclusivity`
- `cpt-cf-bss-quotas-fr-claim-retention`
- `cpt-cf-bss-quotas-fr-acceptance`
- `cpt-cf-bss-quotas-fr-purchase-replay`
- `cpt-cf-bss-quotas-fr-accepted-history`
- `cpt-cf-bss-quotas-fr-attribution`
- `cpt-cf-bss-quotas-fr-scope-binding`
- `cpt-cf-bss-quotas-fr-new-purchase-isolation`
- `cpt-cf-bss-quotas-fr-complete-evidence`
- `cpt-cf-bss-quotas-nfr-isolation`
- `cpt-cf-bss-quotas-nfr-durability`
- `cpt-cf-bss-quotas-interface-management`
- `cpt-cf-bss-quotas-usecase-buy`
- `cpt-cf-bss-quotas-usecase-change`

**Principles**: `cpt-cf-bss-quotas-principle-single-authority`.

### 1.3 Actors

| Actor | Role in feature |
| --- | --- |
| `cpt-cf-bss-quotas-actor-buyer` | Start or qualify the operation within the authorized scope |
| `cpt-cf-bss-quotas-actor-commerce` | Provide authoritative results and evidence for recovery |

### 1.4 References

- **PRD**: [Product requirements](../PRD.md).
- **Design**: [Architecture and contracts](../DESIGN.md), §3.3.2–3.3.3 and 3.6.1.
- **Decomposition**: [Feature allocation](../DECOMPOSITION.md).
- **Dependencies**: [Plan quota authoring and acceptance](0002-plan-rules.md).
- **Pending owner contracts**: [Entitlements #5744](https://github.com/constructorfabric/gears-rust/pull/5744). All `ENT-D-*` and `E-U*` references depend on this pending work, as described in DESIGN §1.2.

## 2. Actor Flows (CDSL)

### 2.1 Purchase attribution and quota exclusivity

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-flow-purchase`

**Actor**: `cpt-cf-bss-quotas-actor-buyer`.

**Use cases**: `cpt-cf-bss-quotas-usecase-change`.

**Success Scenarios**: An authorized, supported request returns the matching result defined by the owner contract.

**Error Scenarios**: Reject invalid or unauthorized input without effects. Recover uncertain external outcomes using their original identity.

**Steps**:

1. [ ] - `p1` - Recover the exact accepted receipt through Pricing and verify selection, quantity and tenant axes. - `inst-receipt`
2. [ ] - `p1` - Resolve the authoritative quota ownership domain for every required binding. - `inst-domain`
3. [ ] - `p1` - Use Subscriptions admission to obtain or recover protection for all required domains across payers before commitment or capture authorization. - `inst-claim`
4. [ ] - `p1` - Leave partial or uncertain claims unresolved. Do not commit the acquisition. - `inst-unknown`
5. [ ] - `p1` - Commit eligible commercial posture through the existing owner lifecycle contract. - `inst-commit`
6. [ ] - `p1` - Deliver effective quantity, accepted provenance and claim evidence through the recoverable posture contract. - `inst-handoff`
7. [ ] - `p1` - Derive each applicable allocation rule once on the retained assignment and resource-owner scope. - `inst-derive`

## 3. Processes / Business Logic (CDSL)

### 3.1 Purchase attribution and quota exclusivity checks

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-algo-purchase`

**Input**: Authorized owner request and the pinned evidence defined in DESIGN §3.3.2–3.3.3 and 3.6.1.

**Output**: A result tied to the request, or an explicit refusal or unresolved outcome.

**Steps**:

1. [ ] - `p1` - Verify complete accepted evidence and all historical policy and mapping references. - `inst-snapshot`
2. [ ] - `p1` - Use Subscriptions' effective committed quantity. Keep original receipt quantities as provenance. - `inst-quantity`
3. [ ] - `p1` - Multiply each distinct `PerPlanUnit` rule once and apply each `Fixed` rule once. - `inst-basis`
4. [ ] - `p1` - Reject incompatible additions, scope changes and duplicate effective bindings before effects. - `inst-compatibility`
5. [ ] - `p1` - Keep the original ownership claim protected until the owner confirms closure and safe reuse of the scope. - `inst-retention`
6. [ ] - `p1` - **RETURN** one complete authorized desired set, or the specific unresolved or refused condition. - `inst-result`

### 3.2 Accepted purchase and effective quantity

Pricing freezes the quota snapshot and price receipt atomically. Orders preserves the receipt, accepted line and original quantity. Subscriptions supplies effective quantity from its committed compatible lots. Entitlements does not sum those lots itself. It multiplies each distinct applicable PerPlanUnit binding once by effective quantity and applies Fixed once.

For a 10-vCPU-per-unit rule, buying one unit and then one compatible extra unit gives effective quantity two. The desired cap becomes 20 on the same assignment. The first receipt still records one. Entitlements cannot use that original quantity as the current total or sum the lots independently. A Fixed(10 vCPU) rule stays at 10 after the same increase. These calculations do not depend on package or usage charging.

Reusing a plan item ID does not prove equivalent semantics. Descriptor, coefficient, unit, scope, window, lifecycle and mapping digests must match.

Orders and Subscriptions own purchase orchestration and financial authorization. Quota derivation uses their accepted evidence and committed posture. It does not decide order waves, activation timing, capture or refunds.

Accepted future starts keep their original quota terms. Catalog retirement or an expired assessment cannot replace them with current terms. Commercial eligibility claims, runtime quota holds and physical capacity reservations are separate. None proves the other two.

### 3.3 Protected purchase admission

Subscriptions owns protected acquisition admission. It must protect every quota ownership domain before a purchase/change commits or authorizes capture. Orders/Workflow carries the matching evidence through its existing acquisition contract. A Pricing receipt or check may freeze compatible terms before this gate. It does not prove domain ownership or authorize fulfillment. This feature adds no activation or payment workflow.

The existing commercial cardinality key includes payer and catalog overlap classification. It cannot alone prove quota exclusivity across payers. Subscriptions must establish an equivalent protected cross-payer key or add a quota-domain admission guard to its contract. All purchase paths must exclude conflicting acquisitions atomically. This requirement does not prescribe a separate quota gear.

If a required set spans several domains, protect the whole set before commitment. Partial or unknown claim acquisition is not success.

The claim stays protected during delayed fulfillment and uncertain outcomes. A timeout, lost apply response or cancellation request cannot free the domain for another acquisition. Release or reuse requires definitive closure and the scope owner's confirmation that reuse is safe. Both must use the same domain fence. BSS retains that confirmation as opaque evidence; it does not inspect resource allocations or calculate remaining capacity. Resource adoption stays outside v1.

G03 requires proof of admission ownership, atomic persistence, recovery and retention. These conditions extend the acquisition contract. They do not prescribe a lease API or resource-cleanup mechanism.

Pricing and Products check only the form of a plan rule's scope value: 1 to 255 printable ASCII bytes for each required dimension (pricing D-541, products P-D-276). Neither resolves an alias or recognizes a wildcard. The scope authority must refuse a wildcard and an unresolved alias, and resolve an alias to its canonical owner ID, before admission. A prior limit that Pricing sends to Products for an increase carries no scope, so this feature's claim keeps the scope of a compatible increase equal to the original acquisition's.

### 3.4 Change and distribution boundaries

The initial profile allows compatible quantity increases under the same binding and acquisition. It refuses numerical cap decreases and base-plan changes before effects.

Numerical cap decreases need a separately agreed commercial and adapter contract before they can be enabled.

An independent OSS restriction may prevent fulfillment without changing the purchased limit. The adapter reports the discrepancy to Entitlements. Suspended subscriptions refuse additions and increases until a separately authorized resume. A newer restriction fences a committed increase that arrives late.

Plans distribute different caps through bindings, not shares of a global provider stock pool. An offer for one provider scope cannot copy its full cap into several independent scopes. Cross-provider pools, parent/child budgets and independent component quantities need a new profile and acceptance contract.

Initial offers cannot sell extra CPU through a second conflicting source. Use a supported compatible quantity change or a separately designed component/add-on contract.

### 3.5 Identity continuity

Each identity has a different purpose:

- `plan_revision_id` identifies offered terms.
- `acceptance_id` identifies frozen purchase evidence.
- Acquisition/lot identities identify commercial ownership and quantity contributions.
- `assignmentId` identifies one applied resource binding.

Entitlements keeps the mapping. The adapter sees only its authorized source/scope and operation identities. A receipt with original quantity one does not change after an increase. Subscriptions supplies effective quantity two with both accepted provenances. Entitlements does not sum purchase messages itself.

Commercial posture revision, Entitlements source revision and adapter observation version are separate sequences. Durable records link them. Their numbers need not match.

For a plan with `PerPlanUnit(10 vCPU)` and `PerPlanUnit(20 GB)` on one binding:

| Stage | Data / result |
| --- | --- |
| Published revision | Two rules, exact definition/unit/profile references, common logical binding and quota digest |
| Initial accepted purchase | Quantity `1`, selected rules and charging terms frozen in one receipt |
| Authorized committed posture | Effective quantity `1`, resource-owner binding and accepted snapshot provenance |
| Entitlements desired set | One assignment containing limits `10 vCPU` and `20 GB`, one saved adapter operation |
| Adapter confirmation | Both required limits are effective and available for the scope, matched to the desired digest. No VM creation is required. |
| Compatible quantity increase | Subscriptions commits effective quantity `2` with new accepted provenance. Entitlements sends `20 vCPU`/`40 GB` on the same assignment with a higher source revision. BSS tracks the matching application result. |

## 4. States (CDSL)

This feature uses the owner states defined in DESIGN. It adds no lifecycle state machine. Only the owning contracts can change availability, draft/accepted status or commercial posture.

## 5. Definitions of Done

### 5.1 Owner-contract conformance

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-dod-purchase`

Owners must implement this behavior and retain passing evidence for every criterion below. Under `DOCS-ONLY` traceability, FEATURE checkboxes and evidence references track implementation. They do not claim that code markers exist or tests have run.

**Implements**: `cpt-cf-bss-quotas-flow-purchase`, `cpt-cf-bss-quotas-algo-purchase`.

**Constraints**: `cpt-cf-bss-quotas-constraint-qualified-profiles`.

**Touches**: AcceptanceRef, PricingAcceptanceV1::acceptance and CommercialPostureV1 quota extensions (DESIGN §3.3.2 and §3.6.1). State retention follows `cpt-cf-bss-quotas-db-owner-state` and the evidence entities in `cpt-cf-bss-quotas-entity-evidence-chain`.

Follow the security, error, storage, task-runtime and API rules in DESIGN §2.2 and §3.3.7 without exceptions. Check scope on reads, writes, replay and recovery. Test concurrent calls, foreign scopes, lost responses and restart wherever the flow has external effects.

Unit tests cover pure validation and derivation. Contract and integration tests cover persistence, permissions and actual adapter effects. This feature specifies no new compile-time flag.

Rollback must preserve accepted history, durable identities and unknown effects. Disabling new sales does not erase existing obligations. Follow DESIGN §3.8 and Feature 4 for observability and recovery. Owners supply executable tests and deployment changes.

## 6. Acceptance Criteria

These criteria have stable verification IDs. Implementation is complete only when their required outcomes are verified. This document records no test results. Business criteria remain in [PRD §9](../PRD.md#9-acceptance-criteria).

| Contract criterion | Relevant condition | Required guarantee |
| --- | --- | --- |
| Q-C10 Ownership conflict | Overlapping subject/metric ownership by another commercial source or operator | No overwrite, deletion, implicit merge or double allowance. Atomic ownership admission refuses or follows an explicitly adopted policy. |
| Q-C40 Commercial protection | Future purchases, catalog retirement, expired assessments, lost drafts and uncertain capture outcomes | Preserve original terms, claims and financial evidence. A timeout or missing result cannot create a new purchase, arbitrary cap, refund or release. Recover through the scoped acquisition and recovery protocol. |
| Q-C62 Pricing-to-adapter provenance | A two-metric purchase with duplicate acquisition/posture delivery, interruptions, compatible quantity additions and lost adapter responses | One immutable receipt feeds the same authorized acquisition. Committed posture supplies effective quantity and accepted provenance. Entitlements derives one complete assignment/set and saves its adapter operation. A receipt or draft cannot directly grant quota. Replay cannot duplicate quantity or allowance. Quantity changes replace caps on the same assignment. The adapter receives no plan, subscription or price data. Matching quota results and provenance feed Entitlements readiness aggregation. |
| Q-C66 Exclusive purchase admission | Concurrent cross-payer/product purchases through adapter aliases, uncertain admission, own-acquisition increases and cancellation during unresolved fulfillment or scope reuse | Canonical domain protection allows at most one conflicting acquisition to commit or capture payment. Replay recovers the original claim. A compatible increase keeps that claim protected. Unknown or partial claims block commitment and remain recoverable. Missing scope authority blocks sale. Claim evidence follows posture and assignment and cannot be reused for another domain. Timeout or cancellation cannot release the claim before definitive fenced closure and proof of safe reuse. The adapter also rejects conflicting ownership. |
| Q-C68 Snapshot completeness | Embedded/paginated sets with missing pages, duplicate members, wrong count/digest or mixed snapshot identity, versus an authorized complete empty target | Incomplete or inconsistent evidence cannot publish a candidate as complete, derive or apply a partial allowance, or imply member removal. Retry recovers one immutable snapshot and verifies its canonical whole-set digest and count. An explicit complete empty target follows authorized restriction/revocation rules. A truncated collection cannot act as an empty target. |
