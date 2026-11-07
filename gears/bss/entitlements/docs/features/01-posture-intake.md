---
description: "Consume CommercialPostureV1 from Subscriptions, fence it by revision and digest, and journal every accepted, stale, replayed or conflicting delivery."
---

# Feature: Posture Intake

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Receive and fence a posture revision](#receive-and-fence-a-posture-revision)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Intake fences](#intake-fences)
- [4. States (CDSL)](#4-states-cdsl)
  - [Intake state](#intake-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Fenced posture intake](#fenced-posture-intake)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-featstatus-posture-intake-implemented`
- [ ] `p1` - `cpt-cf-bss-entitlements-feature-posture-intake`

**Status:** proposed implementation; provider contracts and runtime conformance remain delivery gates.

## 1. Feature Context

### 1.1 Overview

Entitlements receives each committed commercial posture revision from the Subscriptions outbox or recovery read, authenticates its producer, compares it with the intake journal, and persists the accepted posture together with the derivation work for every enabled capability in one local transaction. Intake is the only place in this gear where the subscription identity is seen; everything downstream works by scope, assignment and revision.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-entitlements-fr-posture-intake`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-entitlements-actor-subscriptions` | Publishes `CommercialPostureV1` and reads intake state per revision. |
| `cpt-cf-bss-entitlements-actor-platform-operator` | Inspects stale, replayed and conflicting deliveries. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [commercial posture contract](../DESIGN.md#331-commercial-posture-contract).
- **Build prerequisites:** as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md); decisions in [DECISIONS.md](../DECISIONS.md).
- **Provider prerequisites / integration acceptance:** the `CommercialPostureV1` version agreed with Subscriptions (gate E-U06 in [UPSTREAM_REQS.md](../UPSTREAM_REQS.md)).

## 2. Actor Flows (CDSL)

### Receive and fence a posture revision

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-flow-posture-intake`

**Actor:** Subscriptions, through its outbox delivery or its recovery read.

**Success:** the posture revision is accepted and its derivation work is committed to the outbox, or the recorded intake result of an earlier delivery is returned.

**Errors:** unauthenticated producer, tenant scope outside the producer's authority, unsupported contract version, stale revision, digest conflict.

1. Receive `CommercialPostureV1` from the Subscriptions outbox or from a recovery read, as described in the [commercial posture contract](../DESIGN.md#331-commercial-posture-contract). Authenticate the producer and check that the tenant axes in the payload lie within the producer's authorized scope. Refuse an unsupported contract version or an unknown posture value before any journal write; a refused delivery never becomes "no posture".
2. Compare `(orderingTenantId, subscriptionId, revision)` and the payload digest with the intake journal under the [intake fences](#intake-fences). A lower revision is stale: record the delivery with its reason and apply nothing. An equal revision with an equal digest is a replay: return the recorded intake result and do no new work. An equal revision with a different digest is a conflict: refuse it, record both digests and open a visible finding. A higher revision is accepted and supersedes the current head; the outcomes of earlier revisions are retained.
3. Validate lifecycleAction and the complete resourceDispositions collection under DESIGN §3.3.1. Validate activation resolution by authenticated operation/preparation revision and digest. Persist a non-activation generation fence even when no manifest or commitment exists; fence delayed preparation before dispatch and retain earlier generation outcomes for recovery. Process preparation withdrawal and disposition/hold/schedule updates even when capability members or commercial status are unchanged; a missing notice or generation never authorizes deletion.

   Persist the accepted posture as the intake copy and, in the same local transaction, write one derivation work item per enabled capability to the outbox: quota projection, license grants and resource execution, as the accepted snapshot named in the provenance requires. Rules are read from that accepted snapshot, never from the live plan. Quantity is taken from each member; lots are provenance only and are not re-derived.

   A crash after this commit and before dispatch is recovered by outbox replay under the original identity, and no second derivation is created. From this step on, work carries `(namespace, resource tenant, assignmentId)` and the revision; `subscriptionId` does not leave intake.
4. Expose intake state per subscription revision to Subscriptions and to the platform operator: the result (`received`, `accepted`, `superseded` or `conflict`), the stale and replayed deliveries recorded against it, the operation id and the time of the last delivery. An accepted intake is not application; the desired state and the observed provider state are read through the capability features and [reconciliation](06-reconciliation-recovery.md). The read never implies usable access from an accepted posture.

## 3. Processes / Business Logic (CDSL)

### Intake fences

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-algo-intake-fences`

The journal head of a target is the highest accepted revision for `(orderingTenantId, subscriptionId)`. Every delivery is compared with that head by revision first and by digest second. The operation id identifies a retry; it substitutes for neither the revision nor the digest.

| Incoming revision vs head | Digest vs head | Outcome | Journal record |
| --- | --- | --- | --- |
| No head yet | any | Accepted; becomes the first head. | Head with revision, digest, operation id and time. |
| Lower | any | Stale; refused without application. | Stale delivery with reason; head unchanged. |
| Equal | equal | Replay; no new work. | Replay count and last delivery time on the existing result. |
| Equal | different | Conflict; refused, visible until an audited disposition. | Conflict with both digests and reason; head unchanged; finding opened. |
| Higher | any | Accepted; supersedes the head. | New head; the previous head becomes `superseded` and keeps its outcomes. |

The digest is computed over the canonical payload; the canonical rule and the digest algorithm are adopted with the contract version under E-U06. A result that arrives for a revision applies only to that revision; a result for a superseded revision is retained for audit and cannot mark the newer head complete. Journal entries, including stale and conflicting deliveries, are kept for at least the accepted delayed-message and retry horizon stated in [DESIGN §3.7](../DESIGN.md#37-database-schemas--tables); if a stale sender cannot be retired conclusively, the fence stays.

## 4. States (CDSL)

### Intake state

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-state-intake`

A posture revision has intake state `received | accepted | superseded | conflict`. `received` covers the span between producer authentication and the commit of the intake transaction; a crash in that span leaves no accepted posture, and the delivery is replayed. `accepted` means the revision is the head of its target and its derivation work is in the outbox. `superseded` means a higher revision was accepted; the superseded revision keeps its journal record and its outcomes, and late results for it apply only to it.

`conflict` means an equal revision arrived with a different digest; the conflicting delivery is refused, stays visible, never becomes accepted and is closed only by an audited disposition. A stale delivery is a journal record below the head, not a state of a revision.

These are intake observations. They are not subscription statuses, not application states and not a replacement for the state machine of any provider. A later accepted revision supersedes the target but retains earlier outcomes. A refused or lost delivery never silently broadens access and never narrows it either; the desired head is unchanged until a higher revision is accepted.

## 5. Definitions of Done

### Fenced posture intake

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-dod-posture-intake`

Implement producer and tenant scope authentication, the fence table, persistence of the intake copy and its derivation work in one local transaction with the outbox, a journal that survives restart and response loss, and the intake state read per subscription revision. Accept the `CommercialPostureV1` version, the canonical digest rule and the joint replay, conflict and lost-delivery fixtures with Subscriptions under E-U06 before it is enabled. No adapter is called from this feature, and no derivation reads a live plan.

The shared foundation also supplies separate public and capability-plugin SDK contracts, versioned GTS schemas, host registration and durable adapter-binding infrastructure under [DESIGN §3.3.3](../DESIGN.md#333-adapter-capability-profile). Registry and ClientHub access remain in infrastructure; the capability features invoke selection and dispatch. The main gear does not import concrete plugin implementations. This foundation work does not call a provider while accepting posture or require every optional plugin to be installed.

**Implements:** `cpt-cf-bss-entitlements-flow-posture-intake`, `cpt-cf-bss-entitlements-algo-intake-fences`.

## 6. Acceptance Criteria

- [ ] The same revision with the same digest is delivered twice, once more under another transport key: every delivery returns the same intake result, the journal holds one head and exactly one set of derivation work items exists in the outbox.
- [ ] A delivery with an equal revision and a different digest is refused: the head and its derivation work are unchanged, the conflict is visible with both digests in the intake state read and in a finding, and no derivation work is emitted for the refused payload.
- [ ] The outbox delivery of revision N is lost: Entitlements accepts revision N through the recovery read under the same identity and digest, the later arrival of the original delivery is a replay, and a crash after acceptance and before dispatch recovers the same derivation work by outbox replay without a second application.
