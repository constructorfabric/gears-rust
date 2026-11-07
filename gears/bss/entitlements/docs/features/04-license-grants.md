---
description: "Grant definitions, DesiredGrantSet derivation, application through the selected license adapter (LE, native LM or third party) and the seat-assignment boundary."
---

# Feature: License Grants

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Grant application](#grant-application)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Grant derivation](#grant-derivation)
  - [Posture by transition](#posture-by-transition)
- [4. States (CDSL)](#4-states-cdsl)
  - [Desired grant state](#desired-grant-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Fenced grant application through the selected adapter](#51-fenced-grant-application-through-the-selected-adapter)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-featstatus-license-grants-implemented`
- [ ] `p1` - `cpt-cf-bss-entitlements-feature-license-grants`

**Status:** proposed implementation; provider contracts and runtime conformance remain delivery gates.

## 1. Feature Context

### 1.1 Overview

Entitlements derives a desired grant set from the posture members that carry license capabilities and applies it through the selected license adapter (LE, native LM or third party) with scope, assignment and generation identity. Preparation grants are issued while the subscription is `preparing` and reported as readiness observations. Seat assignment, reservation, resets and point-of-use checks stay with enforcement.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-entitlements-fr-license-grants`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-entitlements-actor-license-enforcement` | Selected license adapter (LE, native LM or third party); applies fenced grant state and reports evidence. |
| `cpt-cf-bss-entitlements-actor-subscriptions` | Publishes the posture that selects grants, quantity, validity and holder scope. |
| `cpt-cf-bss-entitlements-actor-platform-operator` | Inspects pending, refused and unknown grant application. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [adapter capability profile](../DESIGN.md#333-adapter-capability-profile).
- **Build prerequisites:** as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md); decisions in [DECISIONS.md](../DECISIONS.md).
- **Provider prerequisites / integration acceptance:** gate E-U02 in [UPSTREAM_REQS.md](../UPSTREAM_REQS.md).

## 2. Actor Flows (CDSL)

### Grant application

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-flow-grant-application`

**Actor:** Subscriptions, through `CommercialPostureV1`; the selected license adapter (LE, native LM or third party) as the effect owner.

**Success:** the `DesiredGrantSet` for the accepted revision is applied and a correlated receipt with status `applied` is stored for every member.

**Errors:** no frozen grant definition for a license member, unsupported capability profile, fence refused, adapter refused, unknown outcome after a lost response, revision superseded before application.

The adapter boundary is provider independent (ENT-D-01): the native License Manager, a third-party licensing system and a customer integration are alternative adapters of one capability profile. Entitlements keeps the desired state, the correlated outcomes and the end-to-end idempotency, stale-action and recovery guarantees; the adapter maps them to its native verbs. A deployment selects the adapter per capability profile; an explicitly inapplicable license capability differs from an unavailable required adapter, and an absent required adapter is never fabricated success.

1. Entitlements accepts a posture revision under [posture intake](01-posture-intake.md). It selects the members whose capability references name a license capability. The other members are handled by [quota projection](03-quota-projection.md) and [resource execution](05-resource-execution.md).
2. For each selected member Entitlements derives one `DesiredGrantSet` member under [grant derivation](#grant-derivation): grant definition reference from the accepted rule snapshot named in the posture provenance, quantity, validity interval, holder scope and posture. The whole set is keyed by `(namespace, resource tenant, assignmentId)`, carries the aggregate source revision, the operation id and a canonical digest, and lists explicit tombstones for members that left the posture. No subscription identifier is written into the set.
3. Entitlements fences the set by source key, revision and digest. A lower revision is stale and refused for dispatch. An equal revision with the same digest recovers the existing set, delivery intent and result without another grant; an equal revision with a different digest conflicts and opens a finding. A higher revision creates the new set, its saved adapter generation and delivery outbox entry in one local derivation transaction consuming the durable intake work.

   Recheck the current source and preparation-generation fences before committing; withdrawn or superseded work cannot initiate stale dispatch. Adapter calls occur only after commit.
4. The outbox dispatches the set to the selected license adapter with scope, assignment identity, desired revision and generation under [grant apply](../DESIGN.md#335-grant-apply). The adapter returns applied, pending, refused or unknown evidence with authoritative effective time where known. Adapter acceptance of the dispatch is not application.
5. Entitlements records an `AppliedReceipt` per member and advances the member under [desired grant state](#desired-grant-state). A pending receipt is recovered by reading or replaying the same application operation; no retry creates another grant. A refused or unknown outcome opens a finding under [reconciliation and recovery](06-reconciliation-recovery.md); it never broadens access.
6. For an initial activation the preparation grants are issued while the subscription is still `preparing` and reported as readiness observations under [initial readiness](02-initial-readiness.md) (ENT-D-02). `active` is not the trigger; preparation rights cannot be derived from `active`, or the cycle would prevent activation. Entitlements owns the assignment manifest; Subscriptions binds its aggregate result to the saved activation operation and posture revision/digest.

**Receipt content.** A receipt carries the same source key, revision and digest, the operation id, the adapter and profile identity, the adapter's own record identity for the grant, the status, the applied effective time where the adapter is authoritative, the current applied revision and digest, and a structured reason. The receipt is the only evidence of application; a dispatch log is not.

## 3. Processes / Business Logic (CDSL)

### Grant derivation

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-algo-grant-derivation`

**Input:** one accepted `CommercialPostureV1` revision and the frozen grant definitions referenced by its provenance. **Output:** a `DesiredGrantSet` for the revision, or a refusal with a structured reason and no dispatch.

1. Require the grant-rule branch of E-U03, including the typed definitions/bindings and Pricing receipt freeze; quota-only acceptance does not close it. Read the grant definition for each license member from the acceptance receipt referenced by the posture provenance, never from the live plan. A member whose receipt holds no grant definition for its capability is refused; absence is not "no grant".
2. Read the frozen definition's quantity mode: `Fixed` applies once per logical grant binding; `PerPlanUnit` multiplies its exact coefficient by the posture member quantity. That quantity is the sum of effective lots computed by Subscriptions; lots are provenance only and Entitlements does not recompute it. Use exact integer conversion within the accepted profile bounds; a fractional, negative or overflowing result refuses the whole set. Retrying or adding a same-binding lot never duplicates a fixed grant.
3. Take the validity interval and the holder scope from the member. A `Restricted` posture keeps the grant with the restriction interval; a `Revoked` posture produces a revoke member, not an absent member. A member present in the previous revision and absent in this one produces an explicit tombstone.
4. Derive the assignment identity within `(namespace, resource tenant)` for each member. Reuse the existing assignment identity when the member identifies the same grant definition and holder scope as the previous revision; retired identities are not reused for a new grant.
5. Select the capability profile and the adapter route for the set. An unknown profile version or an unknown enum variant refuses before application.
6. Compute the canonical digest over the complete member set and tombstones. Two derivations of the same revision produce the same digest; a differing digest for the same revision is a finding.

### Posture by transition

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-algo-grant-by-posture`

| Commercial event | Desired grant posture and boundary |
| --- | --- |
| Initial activation | Preparation grants are issued while the subscription is `preparing` and confirmed usable before Subscriptions commits active; see [initial readiness](02-initial-readiness.md). Entitlements does not issue initial prerequisites for the first time after the commit. Import and resource adoption are deferred. |
| Composition/quantity change | Increases and additions while suspended are refused by Subscriptions before any posture reaches Entitlements. For an applied active change the complete target replaces the previous set at its committed boundary; no transient union of old and new grants. |
| Suspend | Restricted posture that preserves data and the restoration identity of every grant; grants are suspended, not revoked. |
| Resume | The accepted set is restored only after Subscriptions has passed its lifecycle and occupancy guards; the restore carries a newer generation than the suspend. |
| Cancel | Revoke normal service; a separate narrow export grant may exist during permitted retention when the posture names it. |
| Initial credit trial | Prepare usable access under the authenticated operation-bound Billing commitment in posture, retain it across applied T and do not wait for later Billing recording. Pre-T use consumes no bonus; exact profile support is required by E-U07. |
| Trial conversion | Apply the accepted paid posture and retain eligible bonus coverage under its original expiry. Billing recording is not a new access decision; Entitlements applies no trial or grant accounting policy. |

**Purchase-state rule.** Subscriptions requires a separate authorized guarded resume before seats are purchased, quantity is increased or components are added to a suspended subscription. The purchase itself never resumes or queues a deferred grant. If an active purchase committed before the suspension, Entitlements reconciles its pending delivery under the newer restricted revision; the adapter fence must prevent a stale replacement or issue from restoring access.

**Continuity.** Where the product requires no access gap, the selected license adapter must demonstrate that continuity; its replacement or readiness mechanism belongs to the adapter specification. Entitlements cannot promise issue-before-revoke continuity by sending two asynchronous commands. Initial usable readiness is mandatory before initial commercial activation under the joint freshness and withdrawal contract; no synchronous gate is added to resume or to active changes, and circular waiting for a post-activation command is prohibited.

**Seat-assignment boundary.** Quota usage, hard and soft limits, seat binding, reservation atomicity, resets and point-of-use staleness are owned by the enforcement system; the selected adapter materializes the accepted grant definition. Entitlements forwards committed quantity and validity boundaries and reports findings; it does not calculate remaining capacity or reset counters when a recurring fact emits. A quantity-decrease guard that needs occupied-seat evidence requires the enforcement owner's authoritative guard contract, not a cached count in this gear. A named grant never becomes a second source of the same capacity; numerical limits belong to Quota Enforcement.

## 4. States (CDSL)

### Desired grant state

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-state-desired-grant`

Each member of a `DesiredGrantSet` carries one application status: `derived`, `applied`, `refused` or `superseded`. These are application observations of one desired revision, not license lifecycle states and not subscription statuses. The adapter's own grant state machine is read through receipts and never copied as a status here.

| From | Event / to | Mandatory guard |
| --- | --- | --- |
| none | derivation accepted / derived | Set, generation and outbox entry persisted in one transaction; revision higher than the stored one for the source key. |
| derived | receipt `applied` / applied | Receipt matches source key, revision, digest and generation; adapter record identity stored. |
| derived | receipt `refused` / refused | Structured reason stored; finding opened; access is not broadened. |
| derived or applied | newer revision accepted / superseded | The newer set fences this one; late receipts for it are stored as evidence and cannot change access. |
| refused | operator repair with the same revision / derived | Authorized repair replays the same operation identity; no second grant is created. |
| superseded | any later receipt / superseded | Terminal for that revision; retained for reconciliation and cleanup evidence. |

A `pending` or `unknown` receipt keeps the member `derived` and schedules recovery by the same operation identity. An older generation cannot move the current desired head. An authenticated result for the exact older operation updates its own historical evidence and cleanup obligations, without authorizing stale dispatch or restoring access. The read model shows the superseding revision with earlier outcomes retained.

## 5. Definitions of Done

### 5.1 Fenced grant application through the selected adapter

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-dod-license-grants`

The implementation must:

- Derive a `DesiredGrantSet` only from the frozen grant definitions and accepted posture.
- Refuse any member without a definition.
- Persist the set, its generation and outbox entry in one transaction before dispatch.
- Apply it through the selected license adapter (LE, native LM or third party) with scope, assignment identity, desired revision and generation.
- Store a correlated receipt for each member.
- Recover pending and unknown outcomes by the same operation identity without creating another grant.

A delayed issue or resume cannot undo a newer revoke or suspend at the adapter's effect boundary. Preparation grants for an initial activation are issued while `preparing` and reported as readiness observations. No subscription identifier is accepted in or written to any adapter payload; the set is keyed by assignment within `(namespace, resource tenant)`. Seat binding, counting and leases stay with enforcement and are not read from a local counter.

**Implements:** `cpt-cf-bss-entitlements-flow-grant-application`, `cpt-cf-bss-entitlements-algo-grant-derivation`, `cpt-cf-bss-entitlements-algo-grant-by-posture`, `cpt-cf-bss-entitlements-state-desired-grant`.

## 6. Acceptance Criteria

- [ ] After a changed grant definition is published, the original receipt still resolves the original capability, quantity semantics, scope, validity and digest. No live-plan fallback.
- [ ] A profile requiring a grant refuses missing, unsupported or mismatched definition evidence. A quota-only receipt cannot satisfy it.
- [ ] Replaying one acceptance returns the same price, quota and grant snapshots. Mixed old/new snapshots cannot commit, and a grant-semantic change requires a new accepted revision.

- [ ] A posture with two license members and one quota member produces a `DesiredGrantSet` with exactly two members; a license member without a frozen grant definition refuses the whole set with a structured reason and no dispatch.
- [ ] A generation 7 issue delivered after a generation 8 revoke is refused or reconciled at the adapter; its late success callback is stored as evidence and does not restore access.
- [ ] Equal revision and digest recover the same set and result after a crash or lost response; no second grant or delivery identity is created. Equal revision with a different digest conflicts without changing the saved target.
- [ ] A lost adapter response is recovered by reading or replaying the same application operation; the adapter holds one grant for the assignment, not two.
- [ ] Preparation grants for an initial activation exist while the subscription is `preparing` and appear as readiness observations; no grant is issued for the first time after the active commit.
- [ ] Suspend keeps the grant and its restoration identity; a purchase committed before the suspension is reconciled under the newer restricted revision and its late delivery cannot restore access.
- [ ] A quantity decrease that needs occupied-seat evidence is answered by the enforcement owner's guard, not by a count held in Entitlements.
- [ ] No adapter payload, receipt or stored set contains a subscription identifier.
