---
description: "Outbox replay, observation journal, stale-action fencing, findings, operator repair and adapter or profile changes."
---

# Feature: Reconciliation and Recovery

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Reconcile desired and applied state](#reconcile-desired-and-applied-state)
  - [Inspect and repair a mismatch](#inspect-and-repair-a-mismatch)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Recovery matrix](#recovery-matrix)
- [4. States (CDSL)](#4-states-cdsl)
  - [Delivery state](#delivery-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Versioned, recoverable handoff with visible findings](#51-versioned-recoverable-handoff-with-visible-findings)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-featstatus-reconciliation-recovery-implemented`
- [ ] `p1` - `cpt-cf-bss-entitlements-feature-reconciliation`

**Status:** proposed implementation; provider contracts and runtime conformance remain delivery gates.

## 1. Feature Context

### 1.1 Overview

Entitlements replays its outbox with the original request identity and routing, journals every provider observation against its revision, compares desired and applied state, records findings and lets an authorized operator repair through conditional operations only. An older command can never undo a newer revocation; in-flight work stays bound to its original adapter when the adapter or profile changes.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-entitlements-fr-stale-action-fencing`, `cpt-cf-bss-entitlements-fr-reconciliation`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-entitlements-actor-platform-operator` | Requests reconciliation, retries a desired revision, records audited dispositions. |
| `cpt-cf-bss-entitlements-actor-subscriptions` | Reads desired state, last observed state and findings per revision. |
| `cpt-cf-bss-entitlements-actor-quota-enforcement` | Exposes the applied source head for readback. |
| `cpt-cf-bss-entitlements-actor-license-enforcement` | Exposes applied grant evidence for readback. |
| `cpt-cf-bss-entitlements-actor-resource-provider` | Exposes physical outcomes per generation for readback. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [adapter capability profile](../DESIGN.md#333-adapter-capability-profile).
- **Build prerequisites:** as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md); decisions in [DECISIONS.md](../DECISIONS.md).
- **Provider prerequisites / integration acceptance:** fences demonstrated per adapter under gates E-U01, E-U02 and E-U05 in [UPSTREAM_REQS.md](../UPSTREAM_REQS.md).

## 2. Actor Flows (CDSL)

### Reconcile desired and applied state

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-flow-reconcile`

**Actor:** the Entitlements outbox and reconciliation worker; the selected adapters as the sources of applied evidence; Subscriptions as the reader of delivery state.

**Success:** every outstanding desired revision is either confirmed applied by correlated evidence, confirmed refused with a reason, or kept open as a durable finding with an unknown or conflicting outcome.

**Errors:** adapter unavailable, readback denied, evidence for another revision or generation, unexplained higher revision at the provider, adapter or profile changed while work is in flight.

Reconciliation is the same procedure for quotas, grants and resource work orders; the fence it relies on is the provider-independent boundary of ENT-D-01. Entitlements keeps the desired state and the correlated outcomes; the adapter supplies evidence from its native versions, operation status, authoritative state reads or a combination backed by its durable journal. An adapter's dispatch log alone is not proof of external application. Vendor-specific mappings belong to the adapter specification.

1. **Replay the outbox.** Outbox replay uses the same request identity and the saved `AdapterBinding` from [DESIGN §3.3.3](../DESIGN.md#333-adapter-capability-profile). Resolve that exact scoped client and verify contract/profile, stable provider target and observation authority; do not run discovery again to choose a replacement. Retry and backoff change neither identity nor content. A worker that dies after the local commit and before dispatch is recovered by the outbox, which sends the identical desired revision.
2. **Journal every observation.** Every provider observation is persisted against the source key, the desired revision it answers and the operation identity, with the producer, its sequence and the observed time. A result for another revision or generation does not move the current head; it is stored as historical evidence. A stale success cannot mark a newer desired head complete.
3. **Compare complete state.** Reconciliation reads the applied head through the authorized adapter readback: the source head of Quota Enforcement, the applied grant evidence of the selected license adapter (LE, native LM or third party) and the physical outcomes per generation of the resource adapter. It compares the complete desired set, including tombstones, with the applied state. Repair touches only records owned by this source.
4. **Decide per situation.** The [recovery matrix](#recovery-matrix) names the required action for each situation. An unknown outcome is marked unknown and read back; it is never guessed as success or failure.
5. **Record findings.** A missing or mismatched application, a definitive refusal, an unexplained higher revision, a blocked metric and an unknown outcome beyond its recovery horizon each produce a durable finding with the desired state, the last observed state, the source revision, the observed time and the mismatch reason. Subscriptions reads these through the delivery state; a finding shows pending or failed explicitly instead of implied access.
6. **Keep the fence through change.** When the adapter or the profile changes, in-flight work stays bound to its original adapter and profile. A new adapter never replays old commands into a different provider. The transition needs a separately supported settlement before the new adapter owns the source identity.

### Inspect and repair a mismatch

- [ ] `p2` - **ID**: `cpt-cf-bss-entitlements-flow-repair`

**Actor:** an authorized platform operator; Subscriptions as the owner of commercial validity.

**Success:** a finding is closed on correlated evidence or an explicit audited disposition.

**Errors:** operator not authorized for the scope, successor revision not authorized, repair refused by the provider fence, evidence does not match the finding.

1. An authorized operator requests reconciliation for a scope, reading desired and observed state with one consistent tenant scope. Authorization is enforced on recovery reads as well as writes.
2. A missing or mismatched application produces a durable finding and retries the existing desired revision, or submits an explicitly authorized successor revision through Subscriptions. The retry carries the original identity; a successor carries a higher revision.
3. A commercially invalid desired posture is changed in Subscriptions. The operator cannot edit a materialized grant, quota counter or resource through Entitlements; repair runs only through authorized conditional operations at the provider.
4. A finding closes only on correlated evidence of application or on an explicit audited disposition with its author and reason. A consumer outage or a newer desired revision cannot silently erase a historical failed handoff; the superseded revision keeps its findings as history.

## 3. Processes / Business Logic (CDSL)

### Recovery matrix

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-algo-recovery-matrix`

**Input:** one outstanding desired revision, its journal of observations and the adapter readback. **Output:** the next delivery state of that revision and, where required, a finding.

| Recovery situation | Required action |
| --- | --- |
| Remote apply succeeds; local response or write times out | Mark unknown, read the same source key and operation, verify revision and digest and recover the receipt; replay only the same idempotent apply. Do not issue another unkeyed create. |
| Local commit succeeds; worker dies before dispatch | Recover the outbox and send the identical desired revision. |
| Preparation effects or commitment outstanding; activation outcome missing or unavailable | Recover the immutable Subscriptions decision by authenticated operation/preparation revision and digest; applied requires full manifest/commitment matching, while non-activation may precede either and validates any supplied bindings. Retain preparation and escalate while unknown. Deadline expiry requests fenced attempt closure in Subscriptions, not purchase cancellation/refund; recover its exact result. No timeout cleanup or successor while predecessor effects are unsafe. Applied continues the same assignments; proved irreversible non-activation permits authorized cleanup. |
| Delayed old request arrives after revoke | The provider rejects it against the retained tombstone and source head. Local callback filtering is an additional check, not the primary fence. |
| Provider reports a higher revision | Reconcile to known authorized history; mark an unexplained revision as conflict. Do not overwrite it with the local lower head. |
| Record missing, drift or operator edit | Use authoritative source ownership and readback. Repair only through authorized conditional operations; report conflict where another owner or unknown state exists. Never create a parallel record or overwrite an operator record blindly. |
| Metric registration or enforcement mode drifts | Report a blocked update or unknown enforcement finding, retain the commercial obligation and obtain owner remediation. A materialized but inert record never marks the purchased hard-limit profile ready; no metric is substituted silently. |
| Physical outcome partial or unknown, including after supersession | Record each resource with its own outcome by generation; stop stale dispatch separately and keep unresolved effects open. A newer desired head cannot turn unknown or partial execution into refusal. Recover the old operation's actual result without changing the current desired head; timeout is not completion or absence. |
| Adapter or profile changes | Keep in-flight work bound to the original adapter and profile. Require a separately supported transition and settlement; a new adapter never replays old commands into a different provider. |
| Plugin removed, or registry instance exists without a client | Retain the binding and outstanding work, report dependency unavailability and recover through the same binding when restored; do not try a lower-priority provider. |
| Same plugin GTS ID points at a different target/namespace or incompatible contract | Block dispatch and record a binding conflict; neither matching ID nor new metadata authorizes effect replay. Keep unknown outcomes open against the old target. |

Keep tombstones and monotonic heads for at least the entire accepted delayed-message and retry horizon, with an explicit retention and namespace-reuse policy. If stale senders cannot be conclusively retired, retain the fence. Counter retention, tombstone retention and commercial history retention have distinct purposes; garbage collection cannot reopen a revoked source.

## 4. States (CDSL)

### Delivery state

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-state-delivery`

A desired revision has a delivery state `pending`, `acknowledged`, `failed` or `withdrawn` and an observed materialization `pending`, `applied` or `failed`. `withdrawn` is terminal for fresh dispatch of that revision: a cancellation or newer desired revision prevents further stale commands, but does not prove that work already in flight never applied. Retain unknown and partial effects until authoritative recovery establishes their outcome. Late observations update the original operation and its cleanup evidence; they cannot activate, restore access or replace the current desired head.

Claim non-application only when the provider fence or authoritative outcome proves it. These are operation observations, not subscription statuses and not a replacement for the selected adapter's native state machine. A later desired revision supersedes the target but retains earlier outcomes. A failed handoff never silently broadens commercial access.

| From | Event / to | Mandatory guard |
| --- | --- | --- |
| pending delivery | adapter accepted the dispatch / acknowledged | Acceptance of dispatch is not application; materialization stays `pending`. |
| acknowledged | correlated evidence for the same revision and digest / applied materialization | Evidence carries the same source key, revision and digest; a dispatch log is not evidence. |
| pending or acknowledged | definitive refusal or conflict / failed | Recorded with a structured reason and a finding; distinct from unknown. |
| pending, acknowledged or failed | newer revision or cancellation stops fresh dispatch / withdrawn | Preserve outstanding effects and recover their exact original outcomes. Withdrawal alone does not prove non-application; the newer desired head remains authoritative. |
| applied materialization | higher revision applied / materialization remains applied | Mark the old desired revision superseded separately; retain its observed outcome. The newer revision owns the desired head. |

An unknown outcome leaves delivery `pending` before dispatch acknowledgement or `acknowledged` afterwards, unless fresh dispatch has been `withdrawn`. Materialization remains `pending` with per-resource partial/unknown evidence and a finding until readback resolves it. An older revision cannot move the current desired head; authenticated results update only their exact historical operation and cleanup obligations. Each state is readable by source key, operation and revision after restart.

## 5. Definitions of Done

### 5.1 Versioned, recoverable handoff with visible findings

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-dod-reconciliation`

The implementation must:

- Replay the outbox with the original identity and saved routing.
- Journal every observation against its revision and operation.
- Compare complete desired and applied state through authorized adapter readback.
- Apply the recovery matrix without guessing an unknown outcome.
- Record each finding with desired state, last observed state, source revision, observed time and reason.
- Allow authorized operator repair only through conditional operations at the provider. Acknowledgement, applied revision and findings are recorded separately from commercial status.

The stale-action fence is proved end to end for every capability: an older issue, replace, restore or provision cannot undo a newer revoke, restrict or stop at any adapter, including reordered deliveries with different request keys and work already in flight (ENT-D-01). In-flight work stays on its original adapter and profile through a change. Accept each adapter contract and the joint failure and continuity fixture before it is enabled; no local point-of-use check, counter or reset job is reintroduced.

**Implements:** `cpt-cf-bss-entitlements-flow-reconcile`, `cpt-cf-bss-entitlements-flow-repair`, `cpt-cf-bss-entitlements-algo-recovery-matrix`, `cpt-cf-bss-entitlements-state-delivery`.

## 6. Acceptance Criteria

- [ ] An independent provider ceiling changes without a commercial revision: recover its ordered actual restriction/evidence, retain the purchased desired cap, and expose the discrepancy. A successful source-state apply cannot clear it or authorize overwriting operator policy; clear only from authoritative satisfied-enforcement evidence or an explicit commercial change (E-AC39).

- [ ] A crash after the local commit and before dispatch recovers the same adapter command from the outbox without losing the desired revision or duplicating materialization.
- [ ] Delivering issue request N after revoke request N+1 with different command keys cannot restore access; the adapter integration recovers evidence that the newer revision is applied without requiring a vendor-native revision field.
- [ ] A newer desired revision withdraws dispatch while an older license or resource result is unknown or partial. Recovery remains open; a later exact result updates the old operation and cleanup evidence without changing the current desired head.
- [ ] A delayed old result cannot restore the old plan after a newer replacement or cancellation; it is journaled as historical evidence for the superseded revision.
- [ ] The read surface distinguishes accepted at intake, delivery pending, materialization failed and observed success without asserting point-of-use access.
- [ ] A quantity reduction cannot bypass the enforcement owner's occupied-capacity guard; Entitlements keeps no local counter, and the reduction is applied through the provider fence or refused with a finding.
- [ ] No local point-of-use check endpoint, quota reset job or balance logic is required by this feature.
- [ ] An unavailable required adapter yields an explicit gated capability and a visible finding; a product profile that does not require that capability reports not applicable without fabricated application success.
- [ ] An adapter or profile change keeps in-flight work on the original adapter; the new adapter receives no replay of old commands, and the source identity moves only after a supported settlement.

- [ ] After a provider applies a command but its reply is lost, plugin removal or a priority change never sends the command to a second provider. Recovery uses the original binding and source identity.
- [ ] Reusing a plugin GTS ID for a different external target fails binding verification before dispatch; compatible credential rotation to the same authorized target does not create another assignment or source fence.
