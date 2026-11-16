---
description: "Entitlements owns the BSS application boundary for quota, license and resource delivery through selected adapters."
status: accepted
date: 2026-10-23
decision-makers: "BSS Subscriptions and Entitlements owners"
---

Created: 2026-10-23 by Virtuozzo International GmbH
Updated: 2026-11-16 by Virtuozzo International GmbH

# ADR-0001: The Application Layer Is Its Own Gear

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A: thin adapter gear, derivation stays in Subscriptions](#a-thin-adapter-gear-derivation-stays-in-subscriptions)
  - [B: gear owns derivation and application (chosen)](#b-gear-owns-derivation-and-application-chosen)
  - [C: fold into the system gears](#c-fold-into-the-system-gears)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-entitlements-adr-application-boundary`

**Status: accepted.** This records ENT-D-01, ENT-D-02 and ENT-D-03 in [DECISIONS](../DECISIONS.md).

## Context and Problem Statement

Subscriptions owns commercial lifecycle, accepted purchases, renewal and retention eligibility. Entitlements turns those decisions into quotas, license grants and resource work. It also recovers failed or incomplete delivery.

One owner must manage assignment identity, readiness, stale-command protection and recovery across adapters. OSS and service providers keep runtime enforcement, usage accounting and physical execution.

## Decision Drivers

- Give application identity, readiness and recovery one owner.
- Use the same contract for qualified native, third-party and customer integrations.
- Address adapter work by scope, assignment and revision, without subscription IDs.
- Derive from frozen price and entitlement terms, never a live plan.
- Keep activation time T and the commercial lifecycle in Subscriptions.
- Require integration evidence before enabling a profile.

## Considered Options

1. A: a thin adapter gear; derivation stays in Subscriptions.
2. B: a gear that owns derivation and application (chosen).
3. C: put derivation and application in the underlying system gears.

## Decision Outcome

Choose **B: Entitlements owns derivation and application**. This keeps commercial lifecycle separate from delivery while giving every integration the same application boundary.

Entitlements derives desired state from accepted rules and committed posture. It owns the application rule schema, readiness manifest, adapter contracts, correlated results and recovery. Subscriptions owns the commercial decision and commits posture with its lifecycle transition and outbox. It also owns the activation gate and T.

Pricing stores quota and grant rules on `PlanRevision` and freezes them with price terms in one receipt. Products owns the reviewed quota catalog, including versioned metric, unit and scope definitions, qualification and sale availability. Entitlements uses this evidence to derive purchased limits. It does not maintain runtime quota counters or resource allocation/release records.

BSS selects the service route and adapter. Each adapter design defines its provider connections, internal APIs, permissions and application mechanism. The BSS contract requires proof of complete application and protection from stale effects. It does not require one OSS transaction.

The main cross-gear contracts are the `CommercialPostureV1` command from Subscriptions and the `ReadinessObservationV1` result read from Entitlements. A separate `ReadinessChanged` event schedules the read; polling also recovers the result. Subscription identity stays in intake and internal correlation. Adapters receive authorized scope, assignment, operation and revision identities. The quota source key is `(authenticated namespace, resource tenant, assignmentId)`.

### Consequences

- Entitlements tracks desired rights, logical service assignments, application results and recovery. Providers keep runtime enforcement, usage counters and physical resources. BSS contracts require no OSS inventory, resource IDs or per-resource outcomes. An adapter may call a tenant-level or service-level API directly; any native resource mapping stays in its own design.
- Subscriptions exposes the application state reported by Entitlements. Readiness confirmation does not itself commit activation.
- Both owners must agree on the posture and readiness contract versions under E-U06 and Subscriptions S-U04.
- Each capability profile needs qualified adapter evidence. Unavailable required integrations cannot count as success.
- Public reads and repair actions use the shared authorization and audit rules. The boundary adds no security or compliance exception.
- There is one more gear and durable handoff to operate. Each adapter still needs its own mapping, failure recovery and conformance tests.

### Confirmation

Check that all PRD requirements map to DECOMPOSITION and that adapter contracts contain no subscription identifier. Verify that quota runtime accounting stays outside BSS.

Adapter tests must show that delayed old work cannot restore rights after a newer accepted restriction. Lost results must recover through the saved operation and binding. Tests must distinguish complete application from acceptance, partial work and unknown effects.

## Pros and Cons of the Options

### A: thin adapter gear, derivation stays in Subscriptions

Subscriptions derives desired state; a small gear forwards it to adapters.

- Benefit: a smaller application host.
- Cost: Subscriptions must handle provider assignments and derivation alongside commercial state. Keeping provider details out of lifecycle code becomes harder.

### B: gear owns derivation and application (chosen)

Entitlements derives accepted rights and manages their delivery.

- Benefit: one owner for identity, readiness, ordering and recovery across providers.
- Benefit: Subscriptions keeps commercial lifecycle and the activation decision.
- Cost: two versioned inter-gear contracts and another durable service to operate.
- Cost: every adapter profile still needs integration evidence.

### C: fold into the system gears

Each underlying system derives its own rights from commercial posture.

- Benefit: no BSS application gear between posture and enforcement.
- Cost: quota, license and resource systems each interpret purchase evidence. Third-party integrations need their own version of that logic.
- Cost: commercial derivation becomes tied to particular provider implementations.

## More Information

[DECISIONS](../DECISIONS.md) defines readiness, rule ownership and recovery. [UPSTREAM_REQS](../UPSTREAM_REQS.md) records owner contracts and delivery gates.

The [Commercial Quotas design, merged in PR #5973](../../../docs/quotas/DESIGN.md) specifies the quota catalog and application contract. It does not change this ADR's ownership decision.

Revisit the boundary if another owner can provide the same shared derivation and recovery contract. A later decision must preserve accepted terms, saved bindings, source identities and unresolved work during migration.

## Traceability

- **PRD**: [PRD.md](../PRD.md), requirements and integration contracts.
- **DESIGN**: [DESIGN.md](../DESIGN.md), application boundary and adapter contracts.
- **Decisions**: [DECISIONS.md](../DECISIONS.md), ENT-D-01 to ENT-D-03 and ENT-D-11.
- **Gates**: [UPSTREAM_REQS.md](../UPSTREAM_REQS.md), including the owner adoption gates for each enabled profile.
