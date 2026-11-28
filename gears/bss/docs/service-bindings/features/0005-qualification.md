---
description: "Enable a service definition for sale only with adapter-owner evidence for its exact service type and profile versions, and keep accepted purchases when availability ends."
---

# Feature: Qualification and availability


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Qualify and enable](#21-qualify-and-enable)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Availability decisions](#31-availability-decisions)
  - [3.2 Evidence and its limits](#32-evidence-and-its-limits)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Definition availability](#41-definition-availability)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Owner-contract conformance](#51-owner-contract-conformance)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-featstatus-qualification-implemented`
- [ ] `p1` - `cpt-cf-bss-service-bindings-feature-qualification`

## 1. Feature Context

### 1.1 Overview

Record adapter-owner evidence for exact service type and profile versions, and decide the sale availability of service definitions. Owners: Products and adapter owners.

### 1.2 Purpose

This feature covers qualification evidence records, enabling, disabling and withdrawal, and their effect on authoring, publication and acceptance. The adapter design, its OSS tests and resource inventory are out of scope. The owner ruled on 2026-11-25 (SVB-Q01) that qualification records and sale availability are built once, for quota, license and service profiles, after both catalogs exist: this feature and the quotas package's [qualification feature](../../quotas/features/0005-qualification.md) are one build. Until then every service definition stays disabled.

**Requirements**:

- `cpt-cf-bss-service-bindings-fr-availability`
- `cpt-cf-bss-service-bindings-nfr-retention`
- `cpt-cf-bss-service-bindings-contract-resource-adapter`
- `cpt-cf-bss-service-bindings-usecase-buy`

**Principles**: `cpt-cf-bss-service-bindings-principle-single-authority`.

### 1.3 Actors

| Actor | Role in feature |
| --- | --- |
| `cpt-cf-bss-service-bindings-actor-adapter-owner` | Supply and withdraw qualification evidence |
| `cpt-cf-bss-service-bindings-actor-catalog-owner` | Decide availability with a reason |
| `cpt-cf-bss-service-bindings-actor-commerce` | Pricing rechecks availability at each boundary |

### 1.4 References

- **PRD**: [Product requirements](../PRD.md), §5.1.
- **Design**: [Architecture and contracts](../DESIGN.md), §3.3.2.2, §3.5 and §4.2.
- **Decomposition**: [Feature allocation](../DECOMPOSITION.md).
- **Dependencies**: [Service catalog](0001-service-catalog.md) and the quotas package's [catalog feature](../../quotas/features/0001-catalog.md): qualification starts after both catalogs exist.
- **Owner contract**: Entitlements ENT-D-22 and gate E-U05; the quotas package's [qualification feature](../../quotas/features/0005-qualification.md).

## 2. Actor Flows (CDSL)

### 2.1 Qualify and enable

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-flow-qualification`

**Actor**: `cpt-cf-bss-service-bindings-actor-catalog-owner`.

**Use cases**: `cpt-cf-bss-service-bindings-usecase-buy`.

**Success Scenarios**: A definition with matching evidence becomes `enabled`; a disabled or withdrawn one blocks new sales only.

**Error Scenarios**: Missing, mismatched or withdrawn evidence keeps the definition unavailable.

**Steps**:

1. [ ] - `p1` - The adapter owner names the exact adapter version, the service type versions and the profile versions it supports. - `inst-scope`
2. [ ] - `p1` - The adapter owner supplies executed evidence for provision, withdraw where preparation is supported, pause with data kept, restore, service-stop, lost results and stale commands. - `inst-evidence`
3. [ ] - `p1` - Products records an immutable evidence version and links it to the matching definitions. - `inst-record`
4. [ ] - `p1` - A catalog administrator enables a definition with matching evidence, with an audited reason. - `inst-enable`
5. [ ] - `p1` - An administrator disables, or the adapter owner withdraws evidence, which blocks new sales. - `inst-disable`

## 3. Processes / Business Logic (CDSL)

### 3.1 Availability decisions

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-algo-qualification`

**Input**: An availability decision for one definition version.

**Output**: A new availability revision, or a refusal.

**Steps**:

1. [ ] - `p1` - Authorize the caller for the catalog tenant - `inst-authority`
2. [ ] - `p1` - **IF** the decision enables and no evidence version matches the definition's service type and profile versions - `inst-no-evidence`
   1. [ ] - `p1` - **RETURN** `SERVICE_QUALIFICATION_MISSING` without changes - `inst-no-evidence-result`
3. [ ] - `p1` - Append the next availability revision with its state, reason and time, and the evidence reference on enable - `inst-append`
4. [ ] - `p1` - **RETURN** the definition with its new availability - `inst-result`

### 3.2 Evidence and its limits

Evidence names the adapter version, the plugin instance, the exact service type and profile versions, the supported target rules, the measured operating bounds and the adapter design reference. Registration in types-registry, a plugin advertisement or an approved design is never evidence. Evidence for one service type does not cover another, and evidence for an initial installation does not cover an increase.

Availability is separate from content. Disabling or withdrawal blocks submit, approval, immediate publication and acceptance that need the definition; checks show the reason. It does not change published revisions or receipts, does not revoke services and decides no refund. Entitlements keeps delivering and recovering accepted purchases under their frozen bindings.

## 4. States (CDSL)

### 4.1 Definition availability

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-state-availability`

| From | Event / to | Guard |
| --- | --- | --- |
| none | definition approval / `disabled` (`qualification_pending`) | The approval of feature 1 |
| `disabled` | enable / `enabled` | Matching evidence |
| `enabled` | disable / `disabled` | A reason |
| `enabled` or `disabled` | withdraw / `withdrawn` | Evidence withdrawn or a reason; final |

## 5. Definitions of Done

### 5.1 Owner-contract conformance

- [ ] `p1` - **ID**: `cpt-cf-bss-service-bindings-dod-qualification`

Owners implement this behavior and keep passing evidence for every criterion below. Under `DOCS-ONLY` traceability, checkboxes track implementation; they do not claim that tests have run.

**Implements**: `cpt-cf-bss-service-bindings-flow-qualification`, `cpt-cf-bss-service-bindings-algo-qualification`, `cpt-cf-bss-service-bindings-state-availability`.

**Constraints**: `cpt-cf-bss-service-bindings-constraint-first-release`.

**Touches**: Products availability operations and the qualification evidence record (DESIGN §3.3.2.2). State follows `cpt-cf-bss-service-bindings-db-owner-state`.

Availability revisions are append-only and audited. Tests cover enabling without evidence, disabling after publication and acceptance after withdrawal.

## 6. Acceptance Criteria

These criteria have stable verification IDs. Business criteria stay in [PRD §9](../PRD.md#9-acceptance-criteria).

| Contract criterion | Relevant condition | Required guarantee |
| --- | --- | --- |
| SVB-C28 Evidence and enabling | Enabling without evidence; evidence for another service type or profile version; a registry advertisement only | Each keeps the definition unavailable. Only matching evidence enables it, with an audit record. |
| SVB-C29 Disabling and withdrawal | A definition disabled after publication; evidence withdrawn after a purchase | New submit, approval, immediate publication and acceptance are refused (`NOT_SELLABLE` at acceptance); a scheduled switch already approved makes no catalog call, and its sales are refused at acceptance. The published revision, the receipt and the accepted service stay as they are. |
