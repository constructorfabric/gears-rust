---
description: "Provision, pause, restore, stop, export and delete work orders with generation fencing, hold-versus-delete arbitration and truthful physical outcomes."
---

# Feature: Resource Execution

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Resource work order](#resource-work-order)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Generation fencing](#generation-fencing)
- [4. States (CDSL)](#4-states-cdsl)
  - [Work order state](#work-order-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Truthful execution per resource generation](#51-truthful-execution-per-resource-generation)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-featstatus-resource-execution-implemented`
- [ ] `p1` - `cpt-cf-bss-entitlements-feature-resource-execution`

**Status:** proposed implementation; provider contracts and runtime conformance remain delivery gates.

## 1. Feature Context

### 1.1 Overview

Entitlements translates posture into resource work orders with operation identity and immutable resource generation and submits them through the selected resource adapter. The resource provider owns execution and reports actual effective times and pending, unknown or partial outcomes; Entitlements never backdates an outcome and never applies an old order to a replacement resource with a recycled name.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-entitlements-fr-resource-execution`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-entitlements-actor-resource-provider` | Executes work orders and reports physical outcomes per resource generation. |
| `cpt-cf-bss-entitlements-actor-subscriptions` | Owns commercial intent, the confirmed charging cutoff and disposition eligibility. |
| `cpt-cf-bss-entitlements-actor-platform-operator` | Inspects partial and unknown outcomes and hold protection. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [adapter capability profile](../DESIGN.md#333-adapter-capability-profile).
- **Build prerequisites:** as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md); decisions in [DECISIONS.md](../DECISIONS.md).
- **Provider prerequisites / integration acceptance:** gate E-U05 in [UPSTREAM_REQS.md](../UPSTREAM_REQS.md).

## 2. Actor Flows (CDSL)

### Resource work order

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-flow-resource-work-order`

**Actor:** Subscriptions, through `CommercialPostureV1` for a committed revision; the selected resource adapter as the executor.

**Success:** every work order derived from the revision is submitted with its operation identity and immutable resource generation, and a correlated physical outcome with the actual effective time is stored for each resource.

**Errors:** no resource member in the revision, unsupported work order kind at the selected adapter, fence refused, hold protection pending, adapter refused, unknown outcome after a lost response, partial completion, revision superseded before application.

The adapter boundary is provider independent (ENT-D-01): the platform's own resource service, a third-party service system and a customer integration are alternative adapters of one capability profile. Entitlements keeps the desired work, the correlated outcomes and the end-to-end idempotency, stale-action and recovery guarantees; the adapter maps them to its native operations. A deployment selects the adapter per capability profile; an explicitly inapplicable resource capability differs from an unavailable required adapter, and an absent required adapter is never fabricated success.

Subscriptions owns commercial intent, the confirmed charging cutoff and disposition eligibility; the resource provider owns execution and the authoritative physical outcome. Inventory and evidence is a separate role even when the same deployment service supplies both.

1. **Derive.** Entitlements accepts a posture revision under [posture intake](01-posture-intake.md) and selects the members whose capability references name a resource capability. For each member it reads `lifecycleAction`, the applied transition kind and the complete `resourceDispositions` snapshot under [DESIGN §3.3.1](../DESIGN.md#331-commercial-posture-contract), then maps the posture and transition to work order kinds under [generation fencing](#generation-fencing). The mapping below is the complete set of kinds; a transition that needs no resource effect derives no order.

| Posture / transition | Work order kinds | Boundary carried |
| --- | --- | --- |
| `prepare_activation` | Preparation through the readiness manifest | Pinned service profile and logical target; readiness owns dispatch and recovery. The later active revision reuses the prepared assignment/resource mapping and cannot provision a second resource. |
| `Granted` after `Restricted` (resume) | `restore` | The retained resource generation; the same resource is restored, no second resource is created. |
| `Restricted` (suspension) | `pause` | Data and restoration identity preserved; a pause is never deletion. |
| `Revoked` with a confirmed charging cutoff (cancellation, trial expiry) | `service-stop`, then `export` where the frozen disposition terms grant it | The confirmed cutoff as a boundary only; the actual stop time is reported by the adapter. |
| `Revoked`, retention window closed and eligible | `delete` | Exact resource and disposition identity, current deletion schedule generation, expected hold policy version and required prerequisite evidence, as supplied by Subscriptions. |

A disposition-only update is processed even if no capability member changes. Read the complete snapshot, compare its aggregate revision and retain the latest disposition/schedule/hold versions. A hold immediately blocks new destructive dispatch and is propagated to the resource effect owner; report protection only after its fence acknowledges it. A deletion requires explicit `deleteAuthorized`, current eligible schedule and matched prerequisite evidence, not inference from `Revoked`, missing data or an expired local timer.

Logical resource bindings resolve through the saved assignment mapping to the exact immutable generation; ambiguous or mismatched references refuse. Disposition withdrawal is explicit, preserves the fence and cannot erase unknown execution.

2. **Persist.** Each work order carries the operation identity, the kind, the immutable resource generation, the boundary from the table and the adapter and profile it is routed to. A local derivation transaction consumes the durable intake work and writes the orders, desired revision and delivery intent together after checking current source, preparation and disposition/hold fences. No remote call occurs before commit. Entitlements keeps the association from the posture identity to the resource internally; the adapter receives the authorized scope, the resource generation and the operation identity, never a subscription identifier.
3. **Fence and submit.** The outbox dispatches each order to the selected adapter under [generation fencing](#generation-fencing). Retry and backoff change neither identity nor content. A `delete` order is submitted only while the proven hold fence of gate E-U05 is available; without it automatic deletion stays disabled and the order stays `requested` with a finding.
4. **Record the outcome.** The adapter reports the physical outcome per resource and generation with the actual effective time and an explicit `pending`, `applied`, `partial`, `refused` or `unknown` result. Entitlements stores it as it was reported. A stop at 10:05 after a 10:00 cutoff stays 10:05; Entitlements never backdates an outcome to the cutoff and never advances a cutoff to the stop. A dispatch log, a queued command or a broker acknowledgement is not an outcome.

   The outcome is reported to Subscriptions as materialization state; the ordinary customer tariff ends at the confirmed cutoff regardless of the technical delay, and that decision stays with Subscriptions and Billing.
5. **Retention and export.** After a `service-stop`, retained data follows the frozen accepted disposition terms. The adapter exposes the accepted read-only export capability and returns the retention state with effective timing evidence; whether retained data is charged is decided by the financial owner from that evidence, not by the provider and not by Entitlements. Data presence, hold, export or delayed deletion alone creates no fee and does not continue the former service tariff.
6. **Unknown and partial.** A timeout is an unknown outcome, not completion and not absence. Provider inventory absence alone does not prove that this specific deletion completed. A partial multi-resource completion records each resource with its own outcome and keeps the remaining work visible. Unknown and partial outcomes stay open until [reconciliation](06-reconciliation-recovery.md) resolves them by the original identity.

## 3. Processes / Business Logic (CDSL)

### Generation fencing

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-algo-generation-fencing`

**Input:** one work order with its operation identity, kind, resource generation, boundary and current hold evidence; the current desired head for the resource. **Output:** the order is submitted, deferred or refused before any effect, and every late outcome is applied to the right generation or kept as evidence only.

1. **Bind the generation.** Every order names the immutable resource generation it targets. A resource name is not an identity: a later independent purchase can create a new generation under the same name. The adapter must refuse or ignore an order whose generation does not match the resource it finds, and must never apply an old action to a replacement resource with a recycled name.
2. **Order against the desired head.** A lower revision than the applied head is stale and is refused. An equal revision with the same content returns the existing outcome. A higher revision replaces only this source's work after validation. A terminal `delete` outcome closes the generation; a later `provision` for the same acquisition needs a new generation, never the old one.
3. **Pause is not deletion.** A `pause` preserves data and the restoration identity and implements the accepted restricted posture. A `restore` targets the retained generation. An old `restore` already in flight cannot override a newer `pause` or `service-stop`; the fence is at the adapter's effect boundary, not only at a callback filter.
4. **Hold versus delete.** A `delete` carries the current schedule generation and the expected hold policy version. The adapter orders destructive execution against hold placement and withdrawal at its own authoritative execution boundary, or implements acknowledged generation and hold fencing. A cached negative hold read plus an idempotent delete is not enough. When a hold arrives, Entitlements blocks new destructive dispatch at once and distinguishes a requested or propagating hold from confirmed protection at the adapter boundary.

   If the hold wins there, the old delete is durably refused or unapplied. If irreversible deletion already won, the adapter reports its true applied, in-progress or partial state; an already-started destructive winner is reported truthfully, and no one claims that the hold protected or restored the data. The hold stays in force for the remaining protected scope and each resource is reconciled. Unknown ordering blocks any claim of protected or completed disposition until authoritative recovery.
5. **Release is not delete.** When the final applicable hold is released, Subscriptions supplies a fresh schedule generation with a future deadline and the current notice evidence. An old expired deadline, an old notice or an old worker is never reused; a stale worker carrying an earlier schedule generation cannot delete. A newer hold invalidates the new eligibility through the same execution fence.
6. **Recover by identity.** Retry and recovery use the original operation identity and generation. A lost response is read back as the same operation; no second resource is provisioned and no second delete is issued. An old stop success may establish historical runtime evidence, but cannot complete a newer unrelated operation or undo the current posture.

## 4. States (CDSL)

### Work order state

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-state-work-order`

Each work order carries one current execution outcome: `requested`, `applied`, `partial`, `refused` or `unknown`, with per-resource evidence. Dispatch eligibility is separate: `current` or `superseded`. Supersession stops new dispatch of the old action but cannot assert that already dispatched work failed or had no effects. Execution outcomes remain recoverable by the original operation and resource generation; the commercial status stays in Subscriptions.

| From | Event / to | Mandatory guard |
| --- | --- | --- |
| none | order persisted / requested | Written with the desired revision and delivery intent in one local transaction; adapter and profile routing retained. |
| requested | correlated outcome `applied` for the same operation and generation / applied | Actual effective time recorded as reported; a result for another generation does not move this row. |
| requested | some resources applied, others not / partial | Per-resource outcomes recorded; the remaining work stays visible and recoverable by the original identity. |
| requested or unknown | authoritative refusal or non-application fence / refused | Evidence proves the operation had no applied effects and cannot still apply; local supersession alone is not enough. A finding is opened. |
| partial | definitive refusal of remaining resources / partial | Keep confirmed applied effects and record refusal per remaining resource; the aggregate cannot become wholly refused. |
| requested | lost response or timeout / unknown | Not completion, not absence; resolved only by readback or a later correlated outcome. |
| partial | remaining responses lost or timed out / partial | Preserve confirmed effects and mark unresolved resources unknown; recover each by its original identity. |
| requested or unknown | authoritative recovery of the same operation / applied, partial, refused or unknown | Update that operation's evidence even after supersession; wholly refused requires proof of no applied effects. Recovery cannot restore an old desired head or dispatch stale work. |
| partial | authoritative recovery of remaining resources / partial or applied | Preserve confirmed effects; become applied only when every required resource is confirmed applied. Refused or unknown remaining resources stay explicit in the partial result. |
| any execution outcome | higher desired revision supersedes the order / execution outcome unchanged | Mark dispatch eligibility superseded and fence stale effects at the provider. Keep unknown and partial work open until authoritative recovery; the newer desired head does not prove the old physical result. |

A hold arriving while a `delete` is `requested` or `unknown` keeps the order open until the adapter confirms which side won; the order never becomes `applied` on a local receipt alone. A lower desired revision cannot replace the current resource head. A late authoritative outcome still updates its own older operation and its cleanup obligations without completing a newer operation. Each state is readable by resource, generation, operation and revision after restart.

## 5. Definitions of Done

### 5.1 Truthful execution per resource generation

- [ ] `p1` - **ID**: `cpt-cf-bss-entitlements-dod-resource-execution`

The implementation must:

- Derive the revision's complete set of work orders from the posture and lifecycle transition.
- Persist the orders, desired revision and delivery intent in one local transaction.
- Dispatch only through the outbox to the selected adapter.
- Bind each order and outcome to the immutable resource generation.
- Record outcomes with the reported actual effective time, including explicit pending, partial, refused and unknown results.

A pause never deletes; a delete is dispatched only under the proven hold fence of gate E-U05 and is otherwise disabled; a hold racing a delete reaches the fenced outcome and an already-started destructive winner is reported truthfully. No subscription identifier is written to any adapter payload; the adapter receives the authorized scope, the resource generation and the operation identity (ENT-D-01). Enabling the profile requires the ratified adapter mapping and the executed cases named in the [resource provider requirements](../UPSTREAM_REQS.md).

**Implements:** `cpt-cf-bss-entitlements-flow-resource-work-order`, `cpt-cf-bss-entitlements-algo-generation-fencing`, `cpt-cf-bss-entitlements-state-work-order`.

## 6. Acceptance Criteria

- [ ] A superseded pause/stop with a lost response or partial application retains its unknown/partial execution state. Recovery updates the original operation with actual effects and timing without replacing the newer desired head or dispatching stale work; refusal of remaining resources cannot erase confirmed partial effects.

- [ ] A revoked subscription receives newer hold, release and deletion-authorization snapshots without a status change. Lost delivery recovers the same collection; reordering cannot reopen older eligibility. A missing generation, absent notice or omitted disposition never authorizes deletion.
- [ ] A confirmed charging cutoff of 10:00 with a service stop at 10:05 stores 10:05 as the actual stop and reports it to Subscriptions unchanged; the adapter is never asked to backdate, and the tariff boundary stays with Subscriptions.
- [ ] A hold that reaches the adapter before a stale delete prevents the deletion; the reverse order reports the true applied, in-progress or partial deletion without claiming protected or restored data. An unknown order of events keeps the disposition pending until authoritative recovery.
- [ ] A delayed delete for generation A of a resource name does not delete generation B created later under the same name; the new purchase adopts neither A's data nor its continuity.
- [ ] A lost delete response, or a delete that completed only some resources, is recovered by the original operation identity with per-resource outcomes; no second delete is issued and no false completion is recorded.
- [ ] A pause preserves data and the restoration identity; a restore targets the same generation; an old restore in flight cannot override a newer pause or stop. A delete without the proven hold fence stays `requested` with a finding.
