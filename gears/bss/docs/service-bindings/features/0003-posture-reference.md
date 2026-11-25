---
description: "Name the accepted service bindings of each purchased component in the commercial posture and admit only compatible changes."
---

# Feature: Posture service binding reference


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Reference the accepted bindings](#21-reference-the-accepted-bindings)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Change admission](#31-change-admission)
  - [3.2 Reference rules](#32-reference-rules)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Owner-contract conformance](#51-owner-contract-conformance)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-featstatus-posture-reference-implemented`
- [ ] `p1` - `cpt-cf-bss-service-bindings-feature-posture-reference`

## 1. Feature Context

### 1.1 Overview

Carry the accepted service bindings of each purchased component in `CommercialPostureV1` and refuse changes that the frozen bindings cannot serve. Owner: Subscriptions. The owner ruled on 2026-11-25 that this change is built together with the Entitlements service fulfillment feature, as the readiness profile reference was built with initial readiness.

### 1.2 Purpose

This feature covers `serviceBindingRefs` on posture grants, the snapshot check at acquisition, and the admission of increases, decreases and other changes for components with bindings. Service meaning, dispositions and second-release authority are out of scope.

**Requirements**:

- `cpt-cf-bss-service-bindings-fr-posture-reference`
- `cpt-cf-bss-service-bindings-fr-compatible-increase`
- `cpt-cf-bss-service-bindings-fr-change-refusal`
- `cpt-cf-bss-service-bindings-nfr-determinism`
- `cpt-cf-bss-service-bindings-interface-owner-sdks`
- `cpt-cf-bss-service-bindings-usecase-buy`
- `cpt-cf-bss-service-bindings-usecase-increase`

**Principles**: `cpt-cf-bss-service-bindings-principle-single-authority`.

### 1.3 Actors

| Actor | Role in feature |
| --- | --- |
| `cpt-cf-bss-service-bindings-actor-buyer` | Buy, increase or change a component |
| `cpt-cf-bss-service-bindings-actor-commerce` | Orders carries the acceptance; Pricing answers the receipt |
| `cpt-cf-bss-service-bindings-actor-fulfillment` | Entitlements receives the posture |

### 1.4 References

- **PRD**: [Product requirements](../PRD.md), §5.4.
- **Design**: [Architecture and contracts](../DESIGN.md), §3.3.5 and §3.3.7.
- **Decomposition**: [Feature allocation](../DECOMPOSITION.md).
- **Dependencies**: [Plan service bindings and acceptance](0002-plan-bindings.md).
- **Owner contract**: the [Subscriptions commercial posture feature](../../../subscriptions/docs/features/05-entitlements.md) and Entitlements E-U06.

## 2. Actor Flows (CDSL)

### 2.1 Reference the accepted bindings

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-flow-posture-reference`

**Actor**: `cpt-cf-bss-service-bindings-actor-buyer`.

**Use cases**: `cpt-cf-bss-service-bindings-usecase-buy`, `cpt-cf-bss-service-bindings-usecase-increase`.

**Success Scenarios**: Every posture revision of a component names exactly its accepted bindings. Once Pricing's change-delta acceptance exists, a compatible increase commits with the same references.

**Error Scenarios**: A broken snapshot, an increase before the change-delta acceptance exists, a changed binding, an unsupported increase or a decrease is refused before commitment, with no posture revision written.

**Steps**:

1. [ ] - `p1` - At acquisition, read the receipt through `PricingAcceptanceV1::acceptance` and verify it, as today. - `inst-receipt`
2. [ ] - `p1` - Run `verify_service_snapshot` on its `service` member; a failure refuses the acquisition as `failed_precondition` (`accepted_terms_corrupt`). - `inst-verify`
3. [ ] - `p1` - Record one reference for each accepted binding: the grant's own acceptance, the binding ID and the binding digest. - `inst-record`
4. [ ] - `p1` - Write the references, ordered by binding ID, on the component's grant in every posture revision. - `inst-publish`
5. [ ] - `p1` - For a change to a component with references, or an addition whose delta selects a binding, apply the admission checks before any effect. - `inst-admit`

## 3. Processes / Business Logic (CDSL)

### 3.1 Change admission

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-algo-change-admission`

**Input**: A change to a component with references, or an addition whose delta selects a binding: an increase or addition with its change-delta acceptance, a decrease, a change of binding, plan or plan revision, or a chain version.

**Output**: Admission with the resulting committed quantity, or a refusal before any effect. Refusals use the Subscriptions convention: a canonical category with a snake_case reason.

**Steps**:

1. [ ] - `p1` - **IF** the change lowers the quantity of a component with a `PerPlanUnit` binding - `inst-decrease`
   1. [ ] - `p1` - **RETURN** `failed_precondition` (`service_decrease_unsupported`) - `inst-decrease-result`
2. [ ] - `p1` - **IF** the change moves the component to another plan or plan revision, or a chain version's legs select different execution digests for one component - `inst-move`
   1. [ ] - `p1` - **RETURN** `invalid_argument` (`service_binding_changed`) - `inst-move-result`
3. [ ] - `p1` - **IF** the change is an increase or an addition and Pricing's change-delta acceptance with a `service` member does not exist yet (SVB-Q04, owner, 2026-11-25) - `inst-no-delta`
   1. [ ] - `p1` - **RETURN** `failed_precondition` (`service_increase_unsupported`) before any effect - `inst-no-delta-result`
4. [ ] - `p1` - Verify the delta's `service` member with `verify_service_snapshot` - `inst-verify-lot`
5. [ ] - `p1` - **IF** its bindings do not match the component's references one for one by service key, SKU and execution digest - `inst-compare`
   1. [ ] - `p1` - **RETURN** `invalid_argument` (`service_binding_changed`) - `inst-compare-result`
6. [ ] - `p1` - **FOR EACH** `PerPlanUnit` binding, require `increase_supported`, else **RETURN** `failed_precondition` (`service_increase_unsupported`) - `inst-increase-support`
7. [ ] - `p1` - **FOR EACH** binding, compute `service_units` at the resulting committed quantity, never at the added quantity, else **RETURN** `invalid_argument` (`service_quantity_invalid`) - `inst-units`
8. [ ] - `p1` - **RETURN** admission; the component keeps its references to its original acceptance - `inst-admit-result`

Steps 4 to 8 apply once Pricing designs the change-delta acceptance (Subscriptions SUB-D-61, S-U16) with a `service` member and Subscriptions opens same-cell increases (S-U08). Until then step 3 refuses every increase and addition that touches a binding. Subscriptions' initial change profile already refuses decreases, removals and plan replacement (its feature 03); steps 1 and 2 apply when that profile opens such changes.

### 3.2 Reference rules

- The references are complete: one for each binding of the component's accepted snapshot, none other. A component bought on a revision without bindings has none, and the member is left out.
- The references never change during the life of the component. Restriction, resume, renewal and stop postures carry them. A disposition update carries them.
- Each reference names the grant's own acceptance. A later delta, once it exists, holds the same execution digest and never replaces the reference.
- The payload digest covers the member. A posture stored before the member existed keeps its form and digest.
- The refusals are commercial refusals of the change request, answered before any lot, quantity or posture revision is written. A refusal never alters an existing component.

Subscriptions computes nothing about service meaning. It uses the `pricing-sdk` functions `verify_service_snapshot` and `service_units` so that its checks equal those of Pricing and Entitlements.

## 4. States (CDSL)

This feature uses the existing subscription, component and posture states. It adds no lifecycle state.

## 5. Definitions of Done

### 5.1 Owner-contract conformance

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-dod-posture-reference`

Owners implement this behavior and keep passing evidence for every criterion below. Under `DOCS-ONLY` traceability, checkboxes track implementation; they do not claim that tests have run.

**Implements**: `cpt-cf-bss-service-bindings-flow-posture-reference`, `cpt-cf-bss-service-bindings-algo-change-admission`.

**Constraints**: `cpt-cf-bss-service-bindings-constraint-first-release`.

**Touches**: `CommercialPostureV1` grants and `ServiceBindingRefV1` (DESIGN §3.3.5, §3.3.7). State follows `cpt-cf-bss-service-bindings-db-owner-state`.

The Subscriptions SDK gains `PostureGrantV1.service_binding_refs` and `ServiceBindingRefV1`, and the posture's canonical form includes them. Tests cover the posture digest with and without the member, acquisition with a broken snapshot, and every admission refusal.

## 6. Acceptance Criteria

These criteria have stable verification IDs. Business criteria stay in [PRD §9](../PRD.md#9-acceptance-criteria).

| Contract criterion | Relevant condition | Required guarantee |
| --- | --- | --- |
| SVB-C21 References at acquisition | A receipt with two bindings; a receipt without a service member; an edited snapshot; a later restriction and stop | The grant names both bindings in order, each with the grant's acceptance. A receipt without bindings gives no member and the old digest. A broken snapshot refuses the acquisition as `failed_precondition` (`accepted_terms_corrupt`). Restriction and stop postures carry the same references. |
| SVB-C22 Increase admission | An increase before the change-delta acceptance exists; after it exists: a delta of a later revision with the same service, a profile without increase support, units over the maximum at the resulting quantity, a lot of 1 on `PerPlanUnit(1)` with `units_min` 5 and a resulting quantity of 6, a `Fixed` binding | Before the change-delta acceptance exists the increase is refused before any effect. After it exists, the same service commits the resulting quantity with the same references, also from a later revision; unsupported increases and out-of-bounds units are refused before commitment; units are judged at the resulting quantity, so the lot of 1 is admitted; a `Fixed` binding keeps its units. |
| SVB-C23 Unsupported changes | A decrease; a move to another revision or plan; a delta whose bindings differ in execution meaning; a chain version whose legs select different bindings | Each is refused with its category and reason before any lot, quantity or posture revision is written. |
