---
description: "Verify the frozen service bindings that a posture names and derive the service target that the resource adapter receives."
---

# Feature: Binding consumption


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Derive a service target](#21-derive-a-service-target)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Evidence checks](#31-evidence-checks)
  - [3.2 The service target](#32-the-service-target)
  - [3.3 Increases and new revisions](#33-increases-and-new-revisions)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Owner-contract conformance](#51-owner-contract-conformance)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-featstatus-consumption-implemented`
- [ ] `p1` - `cpt-cf-bss-service-bindings-feature-consumption`

## 1. Feature Context

### 1.1 Overview

Turn the posture's references and the frozen receipt into a verified service target. Owner: Entitlements, under its [service fulfillment feature](../../../entitlements/docs/features/05-resource-execution.md), which owns assignments, work orders, results and recovery.

### 1.2 Purpose

This feature fixes what Entitlements reads, checks and sends from the provider contracts: the receipt read, the snapshot and reference checks, completeness, the unit computation, the release refusals and the adapter inputs. It adds nothing to the work-order contract.

**Requirements**:

- `cpt-cf-bss-service-bindings-fr-explicit-selection`
- `cpt-cf-bss-service-bindings-fr-complete-evidence`
- `cpt-cf-bss-service-bindings-fr-adapter-boundary`
- `cpt-cf-bss-service-bindings-nfr-isolation`
- `cpt-cf-bss-service-bindings-nfr-determinism`
- `cpt-cf-bss-service-bindings-contract-resource-adapter`
- `cpt-cf-bss-service-bindings-usecase-buy`
- `cpt-cf-bss-service-bindings-usecase-increase`

**Principles**: `cpt-cf-bss-service-bindings-principle-frozen-evidence`.

### 1.3 Actors

| Actor | Role in feature |
| --- | --- |
| `cpt-cf-bss-service-bindings-actor-fulfillment` | Verify, derive and deliver |
| `cpt-cf-bss-service-bindings-actor-operator` | Inspect findings for blocked fulfillment |
| `cpt-cf-bss-service-bindings-actor-adapter-owner` | Receive the service target through the adapter |

### 1.4 References

- **PRD**: [Product requirements](../PRD.md), §5.4 and §5.5.
- **Design**: [Architecture and contracts](../DESIGN.md), §3.3.6–3.3.8.
- **Decomposition**: [Feature allocation](../DECOMPOSITION.md).
- **Dependencies**: [Posture service binding reference](0003-posture-reference.md).
- **Owner contract**: Entitlements features 01 and 05, gates E-U03, E-U05 and E-U06.

## 2. Actor Flows (CDSL)

### 2.1 Derive a service target

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-flow-consumption`

**Actor**: `cpt-cf-bss-service-bindings-actor-fulfillment`.

**Use cases**: `cpt-cf-bss-service-bindings-usecase-buy`, `cpt-cf-bss-service-bindings-usecase-increase`.

**Success Scenarios**: Each referenced binding yields one verified service target for one assignment.

**Error Scenarios**: Broken evidence is a final refusal with a finding; incomplete evidence blocks new fulfillment with a finding; a read without an answer retries.

**Steps**:

1. [ ] - `p1` - Take a posture that grants or widens access, under the Entitlements intake and evidence rules. - `inst-intake`
2. [ ] - `p1` - For each grant with references, read its receipt with the grant's evidence reference (ENT-D-27) and check that every reference names the grant's acceptance. - `inst-read`
3. [ ] - `p1` - Verify the snapshot, each referenced binding digest and each binding's SKU cell against the grant, using the accepted binding's `sku_version`. - `inst-verify`
4. [ ] - `p1` - Check that the references and the snapshot's bindings match one for one. - `inst-complete`
5. [ ] - `p1` - Refuse a deferred policy slot or target rule before any effect. - `inst-release`
6. [ ] - `p1` - Compute the units at the grant's committed quantity. - `inst-units`
7. [ ] - `p1` - Save the service target with its assignment and send the work order through the saved adapter binding. - `inst-dispatch`

## 3. Processes / Business Logic (CDSL)

### 3.1 Evidence checks

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-algo-consumption`

**Input**: A grant with references, its committed quantity and the receipts they name.

**Output**: Verified service targets, or a final refusal, incomplete input or no answer.

**Steps**:

1. [ ] - `p1` - **IF** the grant's evidence reference lacks the catalog tenant or the order line (E-U06) - `inst-reference`
   1. [ ] - `p1` - **RETURN** `evidence_reference_incomplete`; no service work derives - `inst-reference-result`
2. [ ] - `p1` - Read the receipt through `PricingAcceptanceV1::acceptance` as the gear's principal, with the grant's evidence reference (ENT-D-27); the catalog tenant is never taken from the seller axis, the caller or the resource tenant - `inst-receipt`
3. [ ] - `p1` - **IF** the read is denied, not found or unavailable - `inst-no-answer`
   1. [ ] - `p1` - **RETURN** an answer of the moment and read again later - `inst-no-answer-result`
4. [ ] - `p1` - **IF** the terms digest, `verify_service_snapshot`, a reference's acceptance, a binding digest or a SKU cell does not match; the SKU cell compares the accepted binding's `sku_version` (the sale's), not `evidence.sku_version` - `inst-mismatch`
   1. [ ] - `p1` - **RETURN** a final evidence mismatch and open a finding - `inst-mismatch-result`
5. [ ] - `p1` - **IF** a snapshot binding has no reference, or a reference has no binding - `inst-incomplete`
   1. [ ] - `p1` - **RETURN** incomplete input, derive no new work and open a finding - `inst-incomplete-result`
6. [ ] - `p1` - **IF** a policy slot is not `not_applicable`, or the target rule is `member_scope` - `inst-deferred`
   1. [ ] - `p1` - **RETURN** a final refusal before any effect - `inst-deferred-result`
7. [ ] - `p1` - **IF** `service_units` at the committed quantity fails - `inst-units-fail`
   1. [ ] - `p1` - **RETURN** a final refusal and open a finding - `inst-units-result`
8. [ ] - `p1` - **RETURN** one service target for each binding - `inst-result`

### 3.2 The service target

The target holds the service type reference, the profile reference, the units, the parameters with their schema reference and digest, the access policy, the target rule and the logical scope (DESIGN §3.3.6). Its digest is the Entitlements desired digest. It holds no SKU, plan, acceptance, price, payer or subscription identifier. Route selection matches its profile and service type exactly.

A grant with two references yields two targets and two assignments. Two grants never share a target, even with equal bindings. A posture that only restricts or stops uses the retained targets and reads no receipt.

### 3.3 Increases and new revisions

Until Pricing's change-delta acceptance exists, Subscriptions refuses increases of components with bindings before any effect (SVB-Q04), so no increase reaches Entitlements. Once it exists, after a committed increase the same references arrive with a higher committed quantity. Entitlements computes the units again from the saved binding and sends the changed target for the same assignment under a higher source revision. The previous units stay in effect until the adapter confirms the change. A `Fixed` binding gives the same target, so no work follows.

Entitlements never reads a live plan or catalog version. A catalog change, a new plan revision or a changed default reaches only new acceptances.

## 4. States (CDSL)

This feature uses the Entitlements intake, assignment and work-order states. It adds no state.

## 5. Definitions of Done

### 5.1 Owner-contract conformance

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-dod-consumption`

Owners implement this behavior and keep passing evidence for every criterion below. Under `DOCS-ONLY` traceability, checkboxes track implementation; they do not claim that tests have run.

**Implements**: `cpt-cf-bss-service-bindings-flow-consumption`, `cpt-cf-bss-service-bindings-algo-consumption`.

**Constraints**: `cpt-cf-bss-service-bindings-constraint-first-release`.

**Touches**: `PricingAcceptanceV1::acceptance`, `verify_service_snapshot`, `service_units` and the Entitlements resource plugin (DESIGN §3.3.6). State follows `cpt-cf-bss-service-bindings-db-owner-state`.

Entitlements uses the `pricing-sdk` types and functions once feature 2 is merged and vhp-core takes that gears-rust version. Before that it may build against a local mirror of feature 2's frozen shape and digest vectors, which it replaces with `pricing-sdk` before SVB-G06 closes. Its fakes build receipts with those functions. Tests run the shared digest vectors.

## 6. Acceptance Criteria

These criteria have stable verification IDs. Business criteria stay in [PRD §9](../PRD.md#9-acceptance-criteria).

| Contract criterion | Relevant condition | Required guarantee |
| --- | --- | --- |
| SVB-C24 Evidence verification | A reference without the catalog tenant or the order line; a reference that names another acceptance than the grant's; a receipt of another catalog tenant; a wrong terms digest; an edited snapshot; a reference with another binding digest; a binding whose item is not a cell of the grant | A reference without the catalog tenant or the order line is `evidence_reference_incomplete`; each other case is a final mismatch with a finding. No work is derived. Another tenant's receipt is not read. |
| SVB-C25 Completeness | A grant without references whose receipt froze a binding; a reference to a binding the snapshot lacks; a receipt without a service member and no references | The first two block new fulfillment as incomplete input with a finding. The third derives no service work. Missing evidence never means "no service". |
| SVB-C26 Units and increases (after the change-delta acceptance exists) | Quantity 5 then 8 on `PerPlanUnit(1)`; `Fixed(1)` at 5 and 8; units over the maximum | Units 5 then 8 for the same assignment; 1 and no new work for `Fixed`; a refusal with a finding over the maximum. Before the change-delta acceptance exists no increase arrives. |
| SVB-C27 Adapter input and release | A dispatched target; a binding with a retention slot set; a `member_scope` binding | The adapter receives only the inputs of DESIGN §3.3.6. Deferred items are refused before any effect. Digest vectors match Pricing's. |
