---
description: "Fixed subscription tenant identities, authorization and historical isolation; transfer and import remain deferred."
---

# Feature: Tenant Ownership and Authorization

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Request, approve and commit a transfer](#request-approve-and-commit-a-transfer)
  - [Import a legacy subscription](#import-a-legacy-subscription)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Financial responsibility through transfer](#financial-responsibility-through-transfer)
  - [Hierarchy, privacy and audit](#hierarchy-privacy-and-audit)
  - [Adoption completeness](#adoption-completeness)
- [4. States (CDSL)](#4-states-cdsl)
  - [Transfer and import state](#transfer-and-import-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Authorized tenant scope](#authorized-tenant-scope)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-featstatus-tenancy-import-implemented`
- [ ] `p1` - `cpt-cf-bss-subscriptions-feature-tenancy-import`

**Status:** target design for fixed tenant identity and authorization. Ownership/payer transfer and import/adoption are unavailable.
## 1. Feature Context

### 1.1 Overview

Preserve current authorization and historical financial/resource ownership for the subscription throughout its current lifecycle. Tenant/payer transfer and import are deferred.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-subscriptions-fr-tenant-axes`, `cpt-cf-bss-subscriptions-fr-delegation-proofs`, `cpt-cf-bss-subscriptions-fr-hierarchy-reference`. **Deferred requirement:** `cpt-cf-bss-subscriptions-fr-import`.

**Constraints:** `cpt-cf-bss-subscriptions-constraint-tenant-isolation`, `cpt-cf-bss-subscriptions-constraint-posted-immutability`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-subscriptions-actor-am` | Supplies commercial account references and relationship evidence under the agreed contract. |
| `cpt-cf-bss-subscriptions-actor-tenant-resolver` | Supplies current tenant identity and ancestry through `TenantResolverClient`. |
| `cpt-cf-bss-subscriptions-actor-policy-engine` | Authorizes commands and reads through `PolicyEnforcer`, with PDP-derived scope and verified delegation. |
| `cpt-cf-bss-subscriptions-actor-partner-admin` | Acts within delegated creation-time tenant scope; ownership transfer is deferred. |
| `cpt-cf-bss-subscriptions-actor-platform-operator` | Resolves authorized tenant-scope findings; transfer and import operations are deferred. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [tenant isolation](../DESIGN.md#tenant-isolation-delegation-proofed-cross-tenant).
- **Build prerequisites:** [foundation](01-foundation.md) and [composition](02-composition.md), as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md).
- **Integration acceptance:** the tenant/history core must demonstrate applicable [renewal/dwell](04-renewal-retention.md), [entitlement posture](05-entitlements.md), [commercial fact](08-commercial-facts.md) and [attribution/recovery](09-consumer-contracts.md) contracts for the retained scope. Transfer/import conformance is deferred; these checks do not make consumer features reverse build prerequisites for the tenant/history core.
- **Source decisions:** SUB-D-06 pinned ordering, SUB-D-20 as-of payer and SUB-D-22 dwell lifecycle; accepted S-Q02 occupancy including suspended-place retention, with open owner/consumer contract and exact policy/key movement mappings, with transfer-specific Billing integration deferred.

## 2. Actor Flows (CDSL)

### Request, approve and commit a transfer

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-flow-transfer-tnt`

**Deferred.** Payer/resource-owner transfer is outside the current version. No transfer endpoint, approval flow, claim handover or provider rebind is implemented. Creation-time tenant axes and authorization remain required.

### Import a legacy subscription

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-flow-import`

**Deferred.** Refuse import before creating subscription state, claims or provider effects.

## 3. Processes / Business Logic (CDSL)

### Financial responsibility through transfer

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-transfer-billing`

**Deferred.** No payer-transfer splitting or financial reallocation algorithm is implemented. Existing facts retain their recorded payer; ordinary financial replay remains unchanged.

### Hierarchy, privacy and audit

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-tenant-reference`

Commercial account and OrgTier roll-ups reference their authoritative AM identity/version. Subscriptions never defines tenant topology. Resolve current tenant identity and required ancestry through `TenantResolverClient`, behind an infrastructure adapter where a narrow domain port is useful. Use `PolicyEnforcer` for current authorization and PDP-derived scope. Keep topology, commercial relationship evidence and permission distinct: ancestry is not proof that a seller may charge a payer or grant access to a resource tenant. The [platform service bindings](../DESIGN.md#34-internal-dependencies) specify resolver reuse and historical-proof limitations.

Authorize commands, historical reads and recovery ranges using current PDP-derived scope and the fixed creation-time tenant axes. Preserve the ordering partition and lifecycle sequence. Receipts/domain events retain relevant actor, reason, scope proof, source revision and correlation provenance for later audit consumers. No transfer approval flow or audit store/query/export subsystem is implemented.

### Adoption completeness

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-algo-import-cutover`

**Deferred.** No adoption/cutover algorithm is part of the current delivery.

## 4. States (CDSL)

### Transfer and import state

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-state-tenancy-import`

Tenant axes remain fixed in the current profile. Transfer and import operation states are deferred; current authorization and original tenant provenance remain observable.

## 5. Definitions of Done

### Authorized tenant scope

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-dod-tenancy-import`

Implement creation-time tenant-axis authorization, current-scope proof checks, pinned ordering and cross-tenant isolation for commands, history and replay. No transfer, import or audit subsystem is part of this definition of done.

**Implements:** `cpt-cf-bss-subscriptions-algo-tenant-reference`. Transfer and import IDs remain deferred.

## 6. Acceptance Criteria

- [ ] Creation binds independently authorized seller, payer and resource tenant axes and a stable ordering tenant.
- [ ] Commands and historical/recovery reads enforce current authorization, including replay, without leaking another tenant's operation or financial history.
- [ ] Transfer/rebinding and import are unavailable in the current profile; generic metadata updates cannot bypass that boundary.
- [ ] Actor, correlation and applicable reason provenance accompany domain events for a future audit consumer; no dedicated audit service or projection is required.

- [ ] A positive ancestry result without the required commercial delegation or PDP authorization cannot authorize a cross-tenant command; denial also applies to replay and recovery reads.
- [ ] An unavailable Tenant Resolver or missing required versioned relationship proof does not produce guessed tenant identities or permission. Historical payer coordinates remain unchanged after a current hierarchy change.
