---
description: "Attach allocation rules to plan revisions and freeze the selected rules with accepted charging terms."
---

# Feature: Plan quota authoring and acceptance


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Plan quota authoring and acceptance](#21-plan-quota-authoring-and-acceptance)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Plan quota authoring and acceptance checks](#31-plan-quota-authoring-and-acceptance-checks)
  - [3.2 Quantity and charging examples](#32-quantity-and-charging-examples)
  - [3.3 Draft replacement and publication](#33-draft-replacement-and-publication)
  - [3.4 Qualified change validation](#34-qualified-change-validation)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Owner-contract conformance](#51-owner-contract-conformance)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-featstatus-plan-rules-implemented`
- [ ] `p1` - `cpt-cf-bss-quotas-feature-plan-rules`

## 1. Feature Context

### 1.1 Overview

Attach allocation rules to plan revisions and freeze the selected rules with accepted charging terms. Owners: Pricing and plan authors.

### 1.2 Purpose

This feature covers draft replacement, concurrent editing, copy/clone, approval, publication and accepted quota evidence. Runtime counters, quota provisioning and billing calculation are out of scope.

**Requirements**:

- `cpt-cf-bss-quotas-fr-plan-rules`
- `cpt-cf-bss-quotas-fr-plan-amount`
- `cpt-cf-bss-quotas-fr-plan-absence`
- `cpt-cf-bss-quotas-fr-plan-revision`
- `cpt-cf-bss-quotas-fr-charging-separation`
- `cpt-cf-bss-quotas-fr-plan-edit`
- `cpt-cf-bss-quotas-fr-plan-edit-conflict`
- `cpt-cf-bss-quotas-fr-plan-governance`
- `cpt-cf-bss-quotas-fr-qualification-recheck`
- `cpt-cf-bss-quotas-nfr-isolation`
- `cpt-cf-bss-quotas-interface-management`
- `cpt-cf-bss-quotas-usecase-buy`
- `cpt-cf-bss-quotas-usecase-change`

**Principles**: `cpt-cf-bss-quotas-principle-single-authority`.

### 1.3 Actors

| Actor | Role in feature |
| --- | --- |
| `cpt-cf-bss-quotas-actor-seller` | Start or qualify the operation within the authorized scope |
| `cpt-cf-bss-quotas-actor-commerce` | Provide authoritative results and evidence for recovery |

### 1.4 References

- **PRD**: [Product requirements](../PRD.md).
- **Design**: [Architecture and contracts](../DESIGN.md), §3.3.1.1–3.3.1.3.
- **Decomposition**: [Feature allocation](../DECOMPOSITION.md).
- **Dependencies**: [Quota catalog onboarding](0001-catalog.md).
- **Pending owner contracts**: [Entitlements #5744](https://github.com/constructorfabric/gears-rust/pull/5744). All `ENT-D-*` and `E-U*` references depend on this pending work, as described in DESIGN §1.2.

## 2. Actor Flows (CDSL)

### 2.1 Plan quota authoring and acceptance

- [x] `p1` - **ID**: `cpt-cf-bss-quotas-flow-plan-rules`

**Actor**: `cpt-cf-bss-quotas-actor-seller`.

**Use cases**: `cpt-cf-bss-quotas-usecase-buy`.

**Success Scenarios**: An authorized, supported request returns the matching result defined by the owner contract.

**Error Scenarios**: Reject invalid or unauthorized input without effects. Recover uncertain external outcomes using their original identity.

**Steps**:

1. [x] - `p1` - Read the draft revision and its selected items. - `inst-read`
2. [x] - `p1` - Select exact approved quota definitions, units and profile conditions from Products. - `inst-select`
3. [x] - `p1` - Enter amounts, rule bases and logical bindings separately from charging terms. - `inst-edit`
4. [x] - `p1` - Submit the complete quota collection using the saved revision version and operation identity. - `inst-replace`
5. [x] - `p1` - Read checks for that exact saved revision and inspect every failed rule or field. - `inst-check`
6. [x] - `p1` - Recheck qualification, then submit and publish through existing approval operations. - `inst-publish`
7. [x] - `p1` - Show the selected quota terms and charging terms with the preview digest. - `inst-preview`
8. [x] - `p1` - Freeze the selected terms and original quantity only after the expected quota digest matches. - `inst-accept`

## 3. Processes / Business Logic (CDSL)

### 3.1 Plan quota authoring and acceptance checks

- [x] `p1` - **ID**: `cpt-cf-bss-quotas-algo-plan-rules`

**Input**: Authorized owner request and the pinned evidence defined in DESIGN §3.3.1.1–3.3.1.3.

**Output**: A result tied to the request, or an explicit refusal or unresolved outcome.

**Steps**:

1. [x] - `p1` - Authorize the caller and target scope for replay or new mutation - `inst-authority`
2. [x] - `p1` - Look up the scoped operation identity and compare the original request content before checking the revision required for a new write - `inst-replay`
3. [x] - `p1` - **IF** the same authorized operation and content already have a recorded result - `inst-known-operation`
   1. [x] - `p1` - **RETURN** the original result without applying it again or requiring its old revision to be current - `inst-replay-result`
4. [x] - `p1` - **IF** an existing operation identity has different content - `inst-conflicting-operation`
   1. [x] - `p1` - **RETURN** a content conflict without altering the recorded result - `inst-conflict-result`
5. [x] - `p1` - Validate the new complete replacement and obtain external evidence before the local transaction - `inst-validate`
6. [x] - `p1` - Inside the owner transaction, recheck draft state, revision version and item bindings. Reject a stale new operation without writes - `inst-fence`
7. [x] - `p1` - Commit normalized rules, pinned evidence, digest, audit, version and operation result together. Serialize concurrent requests with the same operation identity through the owner idempotency contract - `inst-commit`
8. [x] - `p1` - **RETURN** the saved collection and new revision version without provisioning quota - `inst-result`

### 3.2 Quantity and charging examples

Before publication, Pricing resolves descriptor/mapping digests, exact conversion bounds, applicability, scope and the required profile. The quota rule extension has its own version and is optional for existing clients. An absent extension means no purchased quota declaration. Products defines technical meaning. The seller sets commercial amounts. Entitlements owns the rule schema, not a second rule repository.

| Example offer | Accepted quantity | Result |
| --- | --- | --- |
| PerPlanUnit(10 vCPU), PerPlanUnit(20 GB) | 1 | 10 vCPU, 20 GB |
| Same rules | 2 | 20 vCPU, 40 GB |
| Fixed(10 resource instances) | 2 | 10 resource instances, not 20 |
| Another plan for same SKU with PerPlanUnit(20 vCPU) | 1 | 20 vCPU only for its own accepted purchase |

`GB = 10^9 bytes` and `GiB = 2^30 bytes` are different units. Conversion to the provider's integer unit must be exact. Never round to fit its RAM quantum. Refuse a 20 GB offer if the provider cannot represent it exactly. A separately authored 20 GiB offer may be valid. Discrete vCPU quantities must be integers. Refuse overflow, fractional integer results and unknown variants before effects.

Package-priced, usage-priced and mixed plans can each produce an OSS cap of 20 (DESIGN §3.3.1). OSS enforces the ceiling. Resource costs follow the accepted charging terms.

The allocation metric vCPU is distinct from the billing meter vCPU-hours. The charging contract must specify the meter, measurement semantics (allocated capacity or utilization), period, rate and included amount. Never infer them from a quota label or current counter.

Exhausting a billing inclusion does not change the quota cap. Any access restriction must come from authorized commercial posture. Usage pricing also does not determine payment timing or imply postpayment.

The form shows the allocation ceiling separately from charging terms and included usage. It reads quota capabilities through Products and charge details through Pricing/usage owner contracts. Acceptance links the quota to frozen charging terms. It creates no second billing meter or included-usage ledger. Neither usage pricing nor charges beyond a billing inclusion permit allocation above the hard cap.

### 3.3 Draft replacement and publication

The editor reads the draft and its items, then lists Products definitions for the item's SKU/version. The author selects exact definitions and enters amounts. The editor PUTs the complete rule set, reads revision checks, and submits through Pricing.

For a 10-vCPU/20-GB plan, both rules use the same resource binding and a compatible profile. Each keeps its own metric, unit and amount. A new revision can offer 20 vCPU/40 GB. Existing accepted purchases keep their earlier terms.

Authorize the revision and every referenced catalog scope. Parse and resolve the complete replacement before changing saved rules. Reject duplicates, invalid references, invalid unit conversions or bounds, and incompatible sets. Get external evidence outside the Pricing transaction. Inside the transaction, recheck draft state, expected revision version and item bindings.

Before hashing, order rules by `rule_id` and normalize decimal amounts exactly. Input array order does not affect grants. Reject unknown fields, null collections and more than 200 rules. Commit rules, pinned evidence, digest, revision-version increment, audit and idempotency result together.

Edits to quota-relevant items, book/dimension bindings and approval state must use the same revision fence. Concurrent item deletion or approval must not leave orphaned rules. Deleting an item with rules returns `ITEM_HAS_QUOTA_RULES`. Remove its rules first. A changed item mapping cannot silently retarget a quota. Checks must reject incompatible bindings.

A known retry with the same scoped operation identity and content returns its original result without applying it again. This holds even when its original `If-Match` is old. Different content under that identity conflicts. A new operation requires the current ETag. Stale writes cannot merge or replace another author's changes.

After an uncertain response, recover through the same operation identity. Read the current revision before making further edits. Replay still requires authorization and follows the owner's idempotency retention contract.

Save and check operations retain qualification and availability evidence with its observation revision and time. This evidence does not serialize every catalog change with Pricing. Submit, approval, publication (including scheduled publication) and new purchase acceptance must each recheck eligibility through owner contracts. A scheduled publication relies on acceptance's recheck of every sale.

An observed withdrawal blocks new use but preserves historical reads. Uncertain dependencies block the guarded action while leaving saved drafts and accepted purchases intact. Authors can read and remove draft rules whose qualification was withdrawn. They cannot resubmit those rules as an eligible replacement. Complete-set validation reports every failure it can determine without dropping a rule.

For known invalid content, checks on a readable revision return HTTP 200, failed check rows and `ready=false`. Transport and dependency failures remain errors. Batch checks use the same quota logic as single checks. Saving rules does not bypass price coverage, SKU reference, quorum or sale-date checks.

### 3.4 Qualified change validation

`operations.apply` declares transport support. `change_support` and qualification determine which commercial changes it can fulfill. `validate_quota_set` receives `change_kind = initial_set | increase | unchanged` and the complete proposed limits. For an increase, it also receives the prior accepted/effective commercial limits. Pricing validates exact final amounts, representable steps, profile constraints and joint application before purchase/change commitment.

If a quantity increase leaves every Fixed cap unchanged, its change kind is `unchanged`. Keep the same scope and assignment. Do not reinstall the quota. BSS does not supply historical occupied quantities.

Qualification must prove that an increase keeps the previous allowance in effect until the full new set is confirmed. An initial-set-only profile can sell its first package, but must refuse additions that raise its caps. A declaration of increase support without conformance evidence cannot enable purchases.

Refuse numerical cap decreases in v1. Lifecycle and security restrictions still follow their separate authorized contract.

## 4. States (CDSL)

This feature uses the owner states defined in DESIGN. It adds no lifecycle state machine. Only the owning contracts can change availability, draft/accepted status or commercial posture.

## 5. Definitions of Done

### 5.1 Owner-contract conformance

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-dod-plan-rules`

Owners must implement this behavior and retain passing evidence for every criterion below. Under `DOCS-ONLY` traceability, FEATURE checkboxes and evidence references track implementation. They do not claim that code markers exist or tests have run.

Implementation evidence: Pricing feature [Plan Quota Rules](../../../pricing/docs/features/plan-quota-rules.md) (D-540 … D-546), with its preview and acceptance (D-547, D-548), and Products feature [Quota Set Validation](../../../products/docs/features/quota-set-validation.md) (P-D-275, P-D-276).

**Implements**: `cpt-cf-bss-quotas-flow-plan-rules`, `cpt-cf-bss-quotas-algo-plan-rules`.

**Constraints**: `cpt-cf-bss-quotas-constraint-qualified-profiles`.

**Touches**: PricingQuotaAuthoringClientV1 and Pricing's quota-rule endpoints, PricingReadV1::resolve and SellabilityV1::check (DESIGN §3.3.1.2 and §3.6.1). State retention follows `cpt-cf-bss-quotas-db-owner-state` and the evidence entities in `cpt-cf-bss-quotas-entity-evidence-chain`.

Follow the security, error, storage, task-runtime and API rules in DESIGN §2.2 and §3.3.7 without exceptions. Check scope on reads, writes, replay and recovery. Test concurrent calls, foreign scopes, lost responses and restart wherever the flow has external effects.

Unit tests cover pure validation and derivation. Contract and integration tests cover persistence, permissions and actual adapter effects. This feature specifies no new compile-time flag.

Rollback must preserve accepted history, durable identities and unknown effects. Disabling new sales does not erase existing obligations. Follow DESIGN §3.8 and Feature 4 for observability and recovery. Owners supply executable tests and deployment changes.

## 6. Acceptance Criteria

These criteria have stable verification IDs. Implementation is complete only when their required outcomes are verified. This document records no test results. Business criteria remain in [PRD §9](../PRD.md#9-acceptance-criteria).

| Contract criterion | Relevant condition | Required guarantee |
| --- | --- | --- |
| Q-C02 Numeric limits | Fractional integer results, overflow, negative amounts, unit mismatch, and zero versus absent/unbounded limits | Reject invalid values before effects and return a specific reason. Accept exact valid results. Keep zero distinct from absent or unbounded limits. |
| Q-C58 Draft rule replacement | Complete-set draft add/edit/remove/clear, foreign items, duplicate effective metrics, unsupported numbers/units, dimensional alternatives and incompatible profiles | Only an authorized unlocked draft changes. All valid rules, pinned evidence, digest, audit, revision version and operation result commit together. Invalid sets have no partial writes. Applicable rules are selected once per requested item/dimension binding, even when prices use fallback. Price amounts and billing inclusions remain unchanged. |
| Q-C59 Concurrent authoring and replay | Competing writes on one ETag, response loss and exact replay, changed-content key reuse, and concurrent item deletion or approval | One write wins. Another operation with a stale ETag conflicts. Same-content replay returns its original result without applying it again. Changed content conflicts. Item and approval writes use the same revision fence. Remove rules before deleting their items. No quota may become orphaned or silently retargeted. |
| Q-C60 Publication, history and copy | Draft changes after checks, qualification withdrawal before publication, and copying/cloning a published revision | Checks identify the evaluated revision, digest, quota rule and field. Submit and publication recheck current evidence. Copies keep exact definitions and correctly remap item IDs. Recheck approval and eligibility. Preserve published/accepted caps and historical reads. Missing historical evidence blocks progress. Do not substitute current definitions. |
| Q-C61 Accepted quota digest | Unchanged prices with changed caps, stale expected quota digest, quota-unaware clients, and acceptance replay after catalog changes | Stale accepted terms fail even when price is unchanged. Quota-unaware callers cannot purchase a quota-bearing plan by omitting its rules. A successful acceptance freezes selected rule/dependency evidence, quota digest, original quantity and charging terms together. Authorized receipt replay returns that same evidence without re-resolving live terms. |
| Q-C65 Qualified increase | Initial-installation-only support, unqualified increase declarations, and a qualified 20-to-40 increase with partial application | Initial-set-only support cannot enable a purchase/change that raises the cap. A support declaration without evidence is not enough. A qualified increase checks all final values and keeps the previous allowance in effect until complete. Refuse voluntary decreases. Lifecycle restrictions follow their separate contract. |
