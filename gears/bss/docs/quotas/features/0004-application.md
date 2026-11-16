---
description: "Deliver a complete desired quota set and recover its outcome without duplicate allowance or stale grants."
---

# Feature: Quota application and recovery


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Quota application and recovery](#21-quota-application-and-recovery)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Quota application and recovery checks](#31-quota-application-and-recovery-checks)
  - [3.2 Ordering and closure](#32-ordering-and-closure)
  - [3.3 Durable application and readiness](#33-durable-application-and-readiness)
  - [3.4 Lifecycle transitions](#34-lifecycle-transitions)
  - [3.5 Recovery matrix](#35-recovery-matrix)
  - [3.6 Persistence and recovery handoff](#36-persistence-and-recovery-handoff)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Application outcome](#41-application-outcome)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Owner-contract conformance](#51-owner-contract-conformance)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-featstatus-application-implemented`
- [ ] `p1` - `cpt-cf-bss-quotas-feature-application`

## 1. Feature Context

### 1.1 Overview

Deliver a complete desired quota set and recover its outcome without duplicate allowance or stale grants. Entitlements and the quota application adapter owners are responsible for this behavior.

### 1.2 Purpose

This feature covers durable intake, ordering, application, quota readiness, lifecycle changes and escalation. BSS sends limits and access posture through the adapter. OSS allocates resources and owns usage, counters and release. Resource cleanup and commercial remedies belong to their owners.

**Requirements**:

- `cpt-cf-bss-quotas-fr-quantity-increase`
- `cpt-cf-bss-quotas-fr-committed-authority`
- `cpt-cf-bss-quotas-fr-readiness`
- `cpt-cf-bss-quotas-fr-readiness-no-resources`
- `cpt-cf-bss-quotas-fr-application-state`
- `cpt-cf-bss-quotas-fr-application-outcomes`
- `cpt-cf-bss-quotas-fr-application-recovery`
- `cpt-cf-bss-quotas-fr-application-copies`
- `cpt-cf-bss-quotas-fr-lifecycle`
- `cpt-cf-bss-quotas-fr-terminal-closure`
- `cpt-cf-bss-quotas-fr-closed-recovery`
- `cpt-cf-bss-quotas-fr-validity`
- `cpt-cf-bss-quotas-fr-financial-separation`
- `cpt-cf-bss-quotas-fr-visibility`
- `cpt-cf-bss-quotas-fr-discrepancy`
- `cpt-cf-bss-quotas-fr-visibility-freshness`
- `cpt-cf-bss-quotas-fr-recovery`
- `cpt-cf-bss-quotas-fr-recovery-authority`
- `cpt-cf-bss-quotas-fr-accepted-ordering`
- `cpt-cf-bss-quotas-fr-refusal-replay`
- `cpt-cf-bss-quotas-fr-superseded-evidence`
- `cpt-cf-bss-quotas-fr-failure-handoff`
- `cpt-cf-bss-quotas-fr-failure-no-remedy`
- `cpt-cf-bss-quotas-fr-provider-outage`
- `cpt-cf-bss-quotas-nfr-isolation`
- `cpt-cf-bss-quotas-nfr-durability`
- `cpt-cf-bss-quotas-nfr-operations`
- `cpt-cf-bss-quotas-interface-management`
- `cpt-cf-bss-quotas-contract-provider`
- `cpt-cf-bss-quotas-usecase-buy`
- `cpt-cf-bss-quotas-usecase-change`
- `cpt-cf-bss-quotas-usecase-end`

**Principles**: `cpt-cf-bss-quotas-principle-single-authority`.

### 1.3 Actors

| Actor | Role in feature |
| --- | --- |
| `cpt-cf-bss-quotas-actor-fulfillment` | Start or qualify an operation within the authorized scope |
| `cpt-cf-bss-quotas-actor-commerce` | Supply committed posture and receive the quota readiness contribution |

### 1.4 References

- **PRD**: [Product requirements](../PRD.md).
- **Design**: [Architecture and contracts](../DESIGN.md), §3.3.3–3.3.5, 3.6–3.8.
- **Decomposition**: [Feature allocation](../DECOMPOSITION.md).
- **Dependencies**: [Purchase attribution and quota exclusivity](0003-purchase.md).
- **Pending owner contracts**: [Entitlements #5744](https://github.com/constructorfabric/gears-rust/pull/5744). All `ENT-D-*` and `E-U*` references point to the pending contracts listed in DESIGN §1.2.

## 2. Actor Flows (CDSL)

### 2.1 Quota application and recovery

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-flow-application`

**Actor**: `cpt-cf-bss-quotas-actor-fulfillment`.

**Use cases**: `cpt-cf-bss-quotas-usecase-end`.

**Success Scenarios**: A supported, authorized request produces a result tied to that request, as defined by the owner contract.

**Error Scenarios**: Invalid or unauthorized requests are refused without effects. Uncertain external outcomes keep their original identity for recovery.

**Steps**:

1. [ ] - `p1` - Verify committed posture, complete quota evidence and domain authority under the owner intake contract. - `inst-intake`
2. [ ] - `p1` - Persist the assignment, complete desired set, saved binding, source revision and delivery intent atomically. - `inst-persist`
3. [ ] - `p1` - Dispatch the operation through the saved selected adapter after commit. - `inst-apply`
4. [ ] - `p1` - Verify operation, source revision, digest and observation identity before recording the result. - `inst-observe`
5. [ ] - `p1` - Contribute only a completely applied correlated set to Entitlements readiness aggregation. - `inst-readiness`
6. [ ] - `p1` - Recover incomplete or uncertain effects under the original operation and binding. - `inst-recover`
7. [ ] - `p1` - Record a durable escalation in Entitlements after definitive refusal or exhausted automatic recovery. Keep the unfulfilled obligation. - `inst-handoff`

## 3. Processes / Business Logic (CDSL)

### 3.1 Quota application and recovery checks

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-algo-application`

**Input**: Authorized owner request and the pinned evidence defined in DESIGN §3.3.3–3.3.5, 3.6–3.8.

**Output**: A result tied to the request, or an explicit refusal or unresolved outcome.

**Steps**:

1. [ ] - `p1` - Reject malformed or unauthorized calls without advancing the accepted head - `inst-authority`
2. [ ] - `p1` - **IF** the operation identity is already recorded - `inst-recorded-operation`
   1. [ ] - `p1` - **RETURN** its recorded or recovered result without reapplying superseded state; reject changed-content replay without overwriting that result - `inst-recover-recorded`
3. [ ] - `p1` - **IF** the source is terminally closed and the new request would grant, increase or resume access - `inst-closed-grant`
   1. [ ] - `p1` - **RETURN** `source_closed` without effects or advancing the accepted head - `inst-terminal`
4. [ ] - `p1` - Reject unseen stale operations and conflicting content at the same revision, as the ordering table requires. For a closed source, allow only authorized updates that preserve closure - `inst-order`
5. [ ] - `p1` - Acknowledge a new valid revision only after the adapter establishes its durable ordering guarantee and prevents delayed older effects - `inst-accept`
6. [ ] - `p1` - Apply the complete desired set through the adapter; increases preserve the prior allowance until complete, while restrictions and revocations deny newly prohibited actions - `inst-effect`
7. [ ] - `p1` - Keep unresolved effects as `pending` or `unknown`. Exhausted retries do not prove refusal - `inst-unknown`
8. [ ] - `p1` - **RETURN** the correlated outcome with distinct accepted and fully applied heads - `inst-result`

### 3.2 Ordering and closure

Ordering and replay must follow these rules:

| Request relative to the highest accepted source revision | Required outcome |
| --- | --- |
| Lower revision, no recorded operation | Refuse as stale. No new effect |
| Replay/recovery of a recorded operation, including an older revision | Return its recorded result or recover unresolved effects without reapplying superseded desired state. Preserve recorded applied/refused outcomes |
| Equal revision, equal desired digest and original operation identity | Recover the same application result, including a definitive refusal, without a fresh attempt |
| Equal revision, different desired digest or conflicting operation identity | Return a conflict. Do not cause a new effect or replace the saved result |
| Higher revision, open source, accepted request | Advance the durable accepted revision before allowing effects. Application may be pending, applied, refused or unknown |
| Grant, increase or resume after terminal source closure | Refuse with `source_closed` even with a higher revision or new operation ID. A new acquisition needs its own authorized identity |
| Non-grant update after terminal source closure | Accept only authorized state that preserves closure and cannot enable allocation. Normal revision/digest rules still apply |
| Authorized read or recovery after terminal source closure | Keep state and original operation outcomes readable. Historical evidence cannot reapply a grant |

For example, revision 7 is accepted and later refused. Revision 5 remains the last fully applied state. An unseen revision 6 is now stale and cannot apply. Replaying revision 7 returns its saved refusal.

A corrected request or a retry after remediation needs a new operation and a source revision above 7, even if the limits are unchanged. Entitlements owns this increasing revision sequence and keeps the same authorized commercial provenance. A higher revision does not create a purchase or change its quantity. Recover an unknown result under its original identity instead of starting a fresh attempt.

Closure is terminal even before initial quota creation. A new member identifier cannot reopen the source.

If Subscriptions closes a preparation generation without activation, its source stays closed. A later attempt for the same acquisition uses a new assignment and source identity for its own generation. It retains the same exclusivity claim, so it is not a conflicting owner. It inherits no allowance. Subscriptions admits it only after the responsible owners confirm that the previous attempt is closed and the scope can be reused (Entitlements ENT-D-07, ENT-D-08). BSS uses that confirmation without examining resource allocations. Recovery must still verify operation identity, revision and content digest separately.

**Closed-source behavior.** Closure prevents further grants, increases and resumes under the old acquisition. It keeps the source, application history and outstanding operations. Authorized reads, recorded-result replay and recovery remain available through the original binding. Recovering a past grant reports its outcome without executing it again. Updates may maintain or strengthen denial, but cannot remove closure or implicitly create a new source.

BSS closure does not delete resources or change runtime counters. Resource lifecycle and accounting remain in OSS.

A new purchase has its own acquisition, assignment and source identities. It inherits no allowance or application result. Adopting existing resources is outside the initial profile. A new identity cannot bypass scope conflicts or replace the required owner confirmation that the scope is safe to reuse. Temporary suspension is different: an authorized resume can restore the same open source under the normal revision rules.

Recovery may resolve `unknown` to `pending`, `applied` or `refused`. Incomplete work may become applied or definitively refused when the relevant conditions hold. Recorded terminal results cannot change. A later outage or older pending observation cannot erase them. A later independent restriction is a separate fulfillment discrepancy, not a change to an applied operation. Entitlements keeps recovery work for superseded operations. Their results cannot complete or reopen the current revision.

Stale revisions, conflicting content and grants to closed sources have structured refusal reasons when rejected without effects. A conflicting replay is rejected without replacing the original operation's result. Recovery still returns that result. Malformed or unauthorized calls use SDK/HTTP errors without creating a false stored application outcome.

### 3.3 Durable application and readiness

Before dispatch, Entitlements saves the desired revision, complete member set, operation identity, selected adapter binding and delivery intent. It submits one operation and recovers it through the same adapter. BSS does not update individual OSS systems, combine their internal receipts or run their compensation steps.

Each adapter result identifies the original operation, source revision and desired digest. A stale result remains history and cannot complete a newer revision. Application confirmation does not override independent restrictions. BSS keeps a fulfillment discrepancy while purchased capacity is unavailable.

Recover an uncertain result through its original operation. A definitive refusal leaves the purchased obligation and reason intact. Retry after remediation follows the higher-revision rules in DESIGN §3.3.4. Pricing and Orders cannot retry by creating another purchase, changing the frozen cap or issuing an implicit refund. Entitlements and the lifecycle/financial owners control recovery and escalation.

Quota confirmation identifies the saved adapter operation, assignment, source revision and desired digest. Queued, partial, unknown or mismatched results cannot establish readiness. DESIGN §3.3.4 defines application and recovery evidence. DESIGN §3.3.5 defines consistent application.

Quota readiness requires no resource creation. Resource deployment included in a purchase has its own fulfillment contract. Entitlements and Subscriptions own preparation, readiness aggregation, activation decisions, deadlines and cancellation/cleanup. This feature defines only the quota evidence used by those processes.

Activation continues the same quota assignment and applied limits. It does not create another allowance. Resource preparation and its runtime accounting are outside this feature.

### 3.4 Lifecycle transitions

Compatible updates keep source/member identities and replace the complete desired set. Renewal, replay and resume do not create another allowance. Changes to metric, kind, unit, subject or window identity are migrations and are refused in v1.

Validity is `[start,end)`. The adapter confirms that the requested limits and access posture apply for the accepted interval. Consumption-window behavior is outside the initial profile.

Restriction is an explicit access posture. The adapter must confirm the requested denial; a stored zero limit or deactivation alone is not enough. BSS does not infer resource deletion or released capacity from this result. Resource actions and accounting follow OSS contracts.

Terminal revocation records permanent acquisition closure. A new purchase gets a new identity without an automatic transfer of allowance, as DESIGN §3.3.4 specifies. Outstanding adapter operations remain recoverable after a newer desired state arrives, without reopening revoked access.

### 3.5 Recovery matrix

| Situation | Required response |
| --- | --- |
| Local commit before worker death | Replay durable intent using the saved identity and adapter binding |
| Remote success, response lost | Mark unknown. Recover source/operation by original identity and verify revision/digest. No fresh create |
| Adapter reports partial application inside OSS | Keep the BSS operation incomplete. Recover through the same adapter and keep the required limits and restrictions in force |
| Equal revision with different content | Conflict finding. No overwrite |
| Provider has unexplained higher head | Investigate authoritative history. Do not overwrite with a local lower revision |
| Old response after newer desired head | Journal old result. Do not complete the new head. Recover old adapter operations |
| Source cap correct, provider ceiling lower | Preserve purchase, report fulfillment discrepancy. No forced overwrite of operator policy |
| Restriction observation lost or reordered | Recover provider restriction cursor independently of commercial revision |
| Descriptor/metric becomes inert | Report unsupported enforcement, block affected readiness and new sales |
| Adapter configuration changes during retry | Retain old routing. No fallback or implicit migration |
| Lifecycle outcome unknown | Keep the existing assignment and unresolved adapter operations. Consume authoritative posture through the Entitlements contract without inventing a new allowance |
| Definitive application refusal or automatic retry exhaustion | Retain the unfulfilled obligation and correlated adapter outcome/reason. Durably hand off to Entitlements recovery/escalation without inventing a lifecycle or financial decision |

### 3.6 Persistence and recovery handoff

Entitlements must persist:

- Tenant-scoped intake uniqueness by producer aggregate/revision, with digest conflict detection.
- One assignment per accepted acquisition, immutable logical binding and preparation generation. Its saved provider scope and exclusivity claim are installed atomically.
- The complete desired set and saved adapter binding for each revision.
- Application operation identity and a compare-and-swap desired head.
- Unique ordered observation identities and finding history that survives supersession.

First creation uses database uniqueness and transactional conflict handling. A read-before-insert check is not enough. Table names follow the owner's `gear__name` convention. Access uses authorized SecureConn calls.

The intake journal and durable derivation work commit together. Derivation rechecks the current head and atomically writes the complete desired set, application operation and outbox. Recording an adapter result and publishing its quota application status also commit together. General readiness storage follows the Entitlements design.

External effects start after commit. No transaction spans a provider call. Commercial, application, observation and tombstone records have different retention needs. Cleanup must never reopen a closed source. Keep terminal fences indefinitely unless an accepted retention policy can conclusively retire all stale senders and identities.

Each enabled profile defines a request deadline, bounded exponential backoff with jitter, concurrency and queue limits, maximum automatic attempts, a freshness threshold and the Entitlements escalation bound. Workers honor cancellation and recover committed work after restart. Fair scheduling stops one failing tenant or provider from exhausting the worker pool.

**Failure handoff boundary.** Entitlements still owns the unfulfilled obligation after a definitive refusal or exhausted automatic retries. It saves the assignment, desired revision, operation identity, pinned binding, latest authoritative outcome and reason, and unresolved-effect evidence. It durably hands this record to recovery/escalation. The handoff must survive restart. A temporary log or notification is not enough.

Stopping retries cannot change `pending` or `unknown` to `refused`. A definitive adapter refusal does not resolve the commercial obligation. BSS reads still expose the application status and reason.

The Entitlements design defines escalation routing, deadlines, operator handling and remediation. ENT-D-14 sets the recovery horizon in the capability profile and names the platform operator as the escalation target. An operator disposition closes only the finding. Orders, Subscriptions and financial owners decide commercial actions through their contracts. This feature adds no cancellation/refund process or automatic capacity release. Acknowledgment or handoff does not prove successful application. Recovery and later attempts still follow DESIGN §3.3.4.

Telemetry covers desired-to-applied lag, incomplete/unknown/refused operations, restriction drift, held preparation age and the reconciliation backlog. Tenant and assignment IDs belong in authorized traces and audit records, not unbounded metric labels. Audit includes the actor, authority, accepted provenance, source revision, operation, adapter result and repair reason. It never includes credentials.

BSS displays purchased and desired limits, application status, confirmation/evidence age and fulfillment discrepancies. OSS read contracts supply operational usage and remaining capacity.

## 4. States (CDSL)

### 4.1 Application outcome

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-state-application-outcome`

**States**: `pending`, `applied`, `refused`, `unknown` as defined in DESIGN §3.3.4. Accepted and applied source heads are separate from operation status. Initial processing records `pending` only after durable acceptance; rejection before effects can return definitive `refused` directly.

**Transitions**:

1. [ ] - `p1` - **FROM** `pending` **TO** `unknown` **WHEN** authoritative outcome evidence is lost - `inst-lose-evidence`
2. [ ] - `p1` - **FROM** `unknown` **TO** `pending` **WHEN** recovery proves known incomplete effects - `inst-recover-partial`
3. [ ] - `p1` - **FROM** `pending` **TO** `applied` **WHEN** the complete correlated change is confirmed - `inst-confirm-complete`
4. [ ] - `p1` - **FROM** `unknown` **TO** `applied` **WHEN** recovery confirms the complete correlated change - `inst-recover-complete`
5. [ ] - `p1` - **FROM** `pending` **TO** `refused` **WHEN** definitive refusal has no unresolved effect or recovery obligation - `inst-refuse-resolved`
6. [ ] - `p1` - **FROM** `unknown` **TO** `refused` **WHEN** authoritative recovery proves definitive refusal with all effects resolved - `inst-recover-refusal`

Terminal results remain immutable history. No retry exhaustion, lost observation or later restriction changes a recorded `applied` or `refused` result. Source closure is an independent permanent grant fence; it does not prevent recovery of original adapter operations.

## 5. Definitions of Done

### 5.1 Owner-contract conformance

- [ ] `p1` - **ID**: `cpt-cf-bss-quotas-dod-application`

The responsible owners must implement this behavior and keep passing evidence for every criterion below. `DOCS-ONLY` traceability uses FEATURE checkboxes and evidence references to track implementation. It does not claim code markers or completed test runs.

**Implements**: `cpt-cf-bss-quotas-flow-application`, `cpt-cf-bss-quotas-algo-application`, `cpt-cf-bss-quotas-state-application-outcome`.

**Constraints**: `cpt-cf-bss-quotas-constraint-qualified-profiles`.

**Touches**: apply_quota_state, read_quota_state and read_operation_result (DESIGN §3.3.4). State retention follows `cpt-cf-bss-quotas-db-owner-state` and the evidence entities in `cpt-cf-bss-quotas-entity-evidence-chain`.

Follow the shared security, error, storage, task-runtime and API rules in DESIGN §2.2 and §3.3.7. This feature adds no exceptions. Check scope on reads, writes, replay and recovery. Test concurrent calls, foreign scopes, response loss and restart wherever the flow has external effects. Owner unit tests cover pure validation and derivation. Contract and integration tests cover persistence, permissions and actual adapter effects. This change defines no new compile-time feature flag.

Rollback must keep accepted history, durable identities and unknown effects. Disabling new sales cannot erase existing obligations. Observability and recovery follow DESIGN §3.8 and Feature 4. The owner implementation supplies test code and deployment changes.

## 6. Acceptance Criteria

These criteria have stable verification IDs. Implementation is complete only when the required outcomes are proven. These are criteria, not test results. Business criteria remain in [PRD §9](../PRD.md#9-acceptance-criteria).

| Contract criterion | Relevant condition | Required guarantee |
| --- | --- | --- |
| Q-C08 Unknown apply | Provider success followed by a lost response and worker restart | Read or replay returns the same applied revision/digest with one effect. Keep the outcome visibly unknown until it is resolved. |
| Q-C09 Accepted revision fence and refusal replay | Applied revision 5, accepted-but-refused revision 7, delayed revision 6 or superseded work, replay after restart, invalid higher requests, corrected revision 8, and grant delivery after closure | The accepted head stays at 7 and the applied head at 5. Older work cannot newly take effect. Keep recorded history and unresolved effects recoverable. Replay 7 returns its refusal without executing again. Invalid, unauthorized or conflicting requests cannot advance the fence. Corrected revision 8 uses a new operation under the same commercial provenance. Closure rejects every later grant. Lost responses, retries and restarts cannot reopen the source. |
| Q-C16 Revocation fallback | Revoked source while parent/default policy would otherwise allow access | The restriction must keep access denied. Deactivation alone is not enough. |
| Q-C17 Atomic set | Partial failure within a multi-member desired update | Apply the set atomically or keep it safely restricted and pending with recovery. Refuse unsupported multi-member profiles. Never report a partial grant as complete. |
| Q-C19 Quota identity change | Subject, metric, kind or period identity change presented as an update | Refuse unsupported migration. A new record cannot create a fresh allowance. |
| Q-C23 Preparation-to-service continuity | Authorized preparation followed by owner-confirmed activation, with delayed or duplicate lifecycle delivery | Keep the source, member identities and applied limits. Activation must not create another allowance or a revoke/recreate gap. Owner contracts supply activation timing and outcome recovery. |
| Q-C25 Unknown lifecycle outcome | Unknown activation/withdrawal outcome followed by an authoritative owner resolution | Keep the assignment and adapter operation identities through uncertainty. Apply the authoritative current posture without duplicating allowance or restoring old grants. This workflow does not decide activation or cancellation. |
| Q-C28 Fulfillment discrepancy recovery | Lost/reordered discrepancy observations during an independent OSS restriction and subsequent recovery | BSS keeps the accepted terms and unresolved discrepancy. It recovers ordered adapter results and clears the discrepancy only with matching recovery evidence. OSS owns its restrictions and counters. BSS does not repair them directly. |
| Q-C29 Restriction after quota readiness | Independent ceiling/security restriction changes after quota confirmation, including during owner-controlled activation | The adapter reports the resulting discrepancy to Entitlements. BSS keeps desired terms. Earlier confirmation cannot hide the restriction. Lifecycle owners still arbitrate activation. Quota handling creates no refund or resource-lifecycle action. |
| Q-C41 Application result contract | Partial application, unknown effects, retry exhaustion, recovery, no-effect stale requests and conflicting replay | Known partial application is pending. Uncertain effects are unknown. Retry exhaustion cannot turn either into refused. Applied requires the complete correlated change. Refused requires a definitive reason and no unresolved effects. Recovery uses the same identity. Conflicting replay cannot overwrite the original result. Old observations cannot erase terminal results or complete a newer revision. Partial application cannot establish readiness. |
| Q-C42 Partial increase | Incomplete 10-to-20 limit increase | Keep the prior allowance of 10 until the full increase is applied. BSS shows incomplete delivery and recovers the same operation. |
| Q-C43 Restriction and stale completion | Suspension/revocation with an older grant or resume in flight across worker restart | The adapter prevents older work from reopening access. An old result cannot complete the newer revision in BSS. Unknown effects remain recoverable. |
| Q-C51 Routing and revision replay | Adapter selection changes during an unknown operation, with an old callback arriving after a new desired head | Recover through the saved adapter/provider binding. No fallback grant or completion of the new head from an old reply. Keep unresolved old effects. |
| Q-C52 BSS application status and freshness | Stale, lost or reordered restriction evidence, including a portal read combining commercial and operational views | BSS returns purchased limits, application status, evidence time and unresolved discrepancies. Stale or missing evidence cannot clear a discrepancy or imply success. OSS contracts supply effective limits and used, reserved and remaining amounts. Entitlements does not. |
| Q-C56 Closed-source recovery | Terminal closure with unknown adapter work, a higher-revision regrant, historical recovery, denial-preserving updates and a new conflicting purchase | A closed source rejects every regrant with `source_closed`. Authorized reads, recovery and denial-preserving updates remain available. Recovery cannot execute a historical grant again. A new purchase has a distinct source, inherits no allowance and cannot bypass scope conflicts. Authorized posture can still resume a temporarily suspended source. |
