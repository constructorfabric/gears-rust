---
description: "Decision to make the application of commercial posture to quota, license and resource providers its own BSS gear, Entitlements, with derivation and adapter contracts owned there."
status: accepted
date: 2026-10-23
decision-makers: "BSS Subscriptions and Entitlements owners"
---

Created:  2026-10-23 by Virtuozzo International GmbH
Updated:  2026-10-23 by Virtuozzo International GmbH

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

> **Status: accepted.** Records the rationale for [`../DECISIONS.md`](../DECISIONS.md)
> ENT-D-01, ENT-D-02 and ENT-D-03.

## Context and Problem Statement

Commercial lifecycle and provider execution have separate responsibilities. Subscriptions owns statuses, operations, accepted lots, commercial facts, renewal and retention eligibility. Provider application derives quotas, grants and resource work from those decisions and must recover its own outcomes.

A provider-independent application boundary needs one owner for assignment identity, readiness, stale-command fencing and recovery. Point-of-use checks, counters and physical execution remain with their respective providers.

## Decision Drivers

* One owner for the adapter boundary, so identity, fencing and recovery rules are stated once and do not drift between drafts.
* Adapters must address work by scope, assignment and revision, never by subscription, so a native, third-party or customer integration can serve the same contract.
* Rules must be frozen once, in the acceptance that also freezes prices, so replay never reads a live plan.
* Subscriptions must keep publishing posture from its atomic lifecycle commit and must keep its consumer-side readiness gate, so T stays a Subscriptions fact.
* Public contracts, provider mappings and implementation evidence must remain distinguishable so documented behavior cannot imply delivered integration.

## Considered Options

1. A: a thin adapter gear; derivation stays in Subscriptions.
2. B: a gear that owns derivation and application (chosen).
3. C: fold derivation and application into the system gears (quota-enforcement, license-enforcement, resource provider).

## Decision Outcome

Chosen option: **B, a gear that owns derivation and application.** The design set `gears/bss/entitlements/docs` assigns these responsibilities to Entitlements:

- Derive provider-desired state from committed commercial posture.
- Define the readiness manifest and observation envelope.
- Own the rule schema and semantics.
- Fence stale actions, reconcile state and record findings.
- Specify adapters for Quota Enforcement, the selected license adapter (LE, native LM or third party), the resource provider and customer integrations.

Subscriptions keeps the commercial posture as a versioned aggregate fact and commits it atomically with the lifecycle transition and outbox. It also keeps the consumer-side initial readiness gate. Pricing stores quota and grant rules on the immutable PlanRevision and freezes them in the same acceptance receipt as the price bindings. Products supplies metric, unit and subject semantics and the versioned mapping to the enforcement registry. Providers own counters, grants, point-of-use decisions and physical resources.

Two contracts connect the gears. Downward, `CommercialPostureV1` is published by Subscriptions with the identity `(orderingTenantId, subscriptionId, revision)`, a digest, tenant axes, members, provenance, lifecycle action and complete resource dispositions with hold/schedule generations; the commercial identifier remains inside the Subscriptions-facing intake and its internal correlation records. Upward, `ReadinessObservationV1` is published by Entitlements with ordering tenant, scope, operation/posture correlation, immutable complete manifest binding, producer sequence and aggregate status; no `subscriptionId` crosses it. The quota source key at the provider becomes `(authenticated namespace, resource tenant, assignmentId)`.

### Consequences

* Entitlements owns provider issuance, grants, observed application state and handoff latency. Subscriptions owns commercial posture publication and exposes application state reported by Entitlements.
* Two cross-gear contracts exist and are versioned bilaterally: `CommercialPostureV1` down and `ReadinessObservationV1` up. Their versions are a gate, E-U06, in the Entitlements upstream requirements, and S-U04 in Subscriptions points at Entitlements as its provider.
* The eight quota decisions and seven readiness gates are tracked in the Entitlements open-question register, ENT-Q01 to ENT-Q15. A profile that depends on an open question stays unavailable.
* Entitlements owns the provider contracts for Quota Enforcement, License Enforcement, Pricing rules, Products metrics and resource execution.
* Both design sets must pass link, structure, language, allocation census and spec-check gates; a census confirms that `subscriptionId` appears in no Entitlements adapter contract.

### Confirmation

* The subscription identifier appears only in the Subscriptions-facing contract (`CommercialPostureV1` intake) and never in any adapter contract.
* An allocation census: eight functional and one non-functional requirement in the Entitlements PRD, each allocated exactly once in DECOMPOSITION.
* A fixture per adapter contract: issue N delivered after revoke N+1 with different request keys cannot restore access at the provider; a lost reply is recovered from the provider's source head.

## Pros and Cons of the Options

### A: thin adapter gear, derivation stays in Subscriptions

* Good: a small application host; Subscriptions owns derivation.
* Bad: provider identity and derivation stay coupled to the commercial aggregate, making provider-independent contracts harder to enforce.

### B: gear owns derivation and application (chosen)

* Good: one owner for identity, fencing, readiness and recovery; Subscriptions shrinks to commercial posture and a consumer gate; rules frozen once in Pricing acceptance; adapters serve one contract regardless of vendor.
* Bad: one more design set to keep consistent; two versioned contracts to agree bilaterally; each provider profile still requires concrete integration contracts and conformance.

### C: fold into the system gears

* Good: no BSS gear between posture and enforcement.
* Bad: couples commercial derivation to native providers; the derivation of what was bought would live in each platform gear and in no third-party or customer integration; Pricing acceptance evidence would be read by enforcement code.

## More Information

[DECISIONS.md](../DECISIONS.md) defines provider independence, initial readiness and rule ownership. [UPSTREAM_REQS.md](../UPSTREAM_REQS.md) records the provider contracts and evidence required before the profile is enabled.

## Traceability

- **PRD**: [`../PRD.md`](../PRD.md) §1.2 (background), §6 (eight functional requirements), §9.2 (the five integration contracts), §17.3 (upstream coverage).
- **Decisions**: [`../DECISIONS.md`](../DECISIONS.md) ENT-D-01, ENT-D-02, ENT-D-03; open questions ENT-Q01 to ENT-Q15.
- **Design**: [`../DESIGN.md`](../DESIGN.md) §1.1 (boundary), §3.3.1 and §3.3.2 (the two contracts), §3.3.3 (adapter capability profile).
- **Gates**: [`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) E-U01 to E-U07.
