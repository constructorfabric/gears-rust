---
description: "Commercial posture derived from committed terms and lifecycle, published to Entitlements, with the readiness and materialization state Entitlements reports."
---

# Commercial Posture: Feature

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Initial activation readiness contract](#initial-activation-readiness-contract)
  - [Apply a commercial posture and record its observations](#apply-a-commercial-posture-and-record-its-observations)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Posture by transition](#posture-by-transition)
  - [Financial boundary of posture](#financial-boundary-of-posture)
- [4. States (CDSL)](#4-states-cdsl)
  - [Entitlement delivery state](#entitlement-delivery-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Commercial posture publication and observation intake](#commercial-posture-publication-and-observation-intake)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-featstatus-entitlements-implemented`
- [ ] `p1` - `cpt-cf-bss-subscriptions-feature-entitlements`

**Status:** Subscriptions owns the commercial posture. Entitlements owns manifests, adapters and provider integration; a capability is enabled only after Entitlements accepts the selected adapter.
## 1. Feature Context

### 1.1 Overview

Derive the commercial posture from committed terms and lifecycle, commit it atomically with the transition, publish it to Entitlements, and expose the readiness and materialization state Entitlements reports.

Subscriptions supplies the desired commercial rights and records the observations Entitlements reports against the matching revision. Entitlements owns the readiness manifest, adapters and provider integration. Subscriptions does not take over materialization or point-of-use enforcement.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-subscriptions-fr-commercial-posture`.

**Principle:** `cpt-cf-bss-subscriptions-principle-idempotent-ordered`. **Constraint:** `cpt-cf-bss-subscriptions-constraint-tenant-isolation`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-subscriptions-actor-partner-admin` | Inspects the desired revision, the latest observed state and findings. |
| Entitlements | System actor. Receives the commercial posture, publishes readiness and materialization observations. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [entitlement contract](../DESIGN.md#335-entitlement-and-retention-contracts).
- **Build prerequisites:** [foundation](01-foundation.md) and [composition](02-composition.md), as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md).
- **Provider prerequisites / integration acceptance:** Entitlements accepts the selected adapter against its capability profile before a capability is enabled. Cross-lifecycle handoff verification is an integration acceptance step, not a build dependency on each lifecycle feature.

## 2. Actor Flows (CDSL)

### Initial activation readiness contract

**Initial readiness (IA-2026-10-18).** Initial activation completes only after all required initial service capabilities are confirmed usable (SUB-D-55). T is the fresh server-assigned instant of the guarded local activation commit. Subscription status stays `draft` while its durable activation operation is pending/approved. Preparation progress is operation state, not a new commercial status. Renewals, resume, trial-to-paid conversion and modifications of an already active subscription retain their existing contracts.

**Consumer gate.** Before publishing preparation posture, Subscriptions persists the activation operation, its authorized scope, expected posture revision and digest, and the outbox intent atomically. Entitlements owns assignment allocation and the complete manifest. `ReadinessObservationV1` is an aggregate result from Entitlements, not a forwarded adapter event: it carries `operationId`, `postureRevision`, `postureDigest`, `manifestId`, `manifestVersion`, `manifestDigest`, the complete `requiredAssignments` roster, `serviceProfileRef`, `scopeId`, `schemaVersion`, `eventId`, `producerId`, `statusSequence`, `status`, `observedAt`, `evidenceRef` and `reasonCode`. Each roster entry identifies scope, assignment, desired revision, target and service profile; producer authority remains Entitlements' responsibility.

The operation id is unique within the authenticated ordering-tenant namespace; the envelope also carries `orderingTenantId` for correlation, never `subscriptionId`.

Subscriptions authenticates Entitlements, matches the saved operation/scope/posture triple and atomically binds the first manifest identity/version/digest to that operation with its observation. The same manifest version cannot change its roster or digest. A replacement requires a higher posture revision from Subscriptions; unsolicited rebinding conflicts. Duplicate event/content is a no-op; equal sequence with different content or a conflicting duplicate event is refused, and older sequences cannot replace newer readiness or withdrawal. A sequence gap requires the authoritative full manifest-result read keyed by operation and posture revision.

Only aggregate `ready` for the exact current binding satisfies activation; no subset of assignment events can do so. Entitlements emits and retains an explicit aggregate result even for an empty manifest, with evidence that the accepted profile requires no preparation. This lets fast callbacks correlate to an already persisted operation without making Subscriptions allocate adapter assignments.

**Held readiness commitment (SUB-D-57).** Before publishing aggregate `ready`, Entitlements durably records a readiness commitment bound to the authenticated ordering namespace, scope, activation operation, preparation posture revision/digest, manifest identity/version/digest and service profile. The aggregate carries `readinessCommitmentId` and its immutable binding/evidence reference. Subscriptions stores that commitment with the manifest association; a timestamp or ready observation without this obligation is insufficient.

The commitment preserves the required prepared assignments through resolution of this exact attempt; ordinary withdrawal, cleanup, adapter replacement or expiry of a housekeeping timer cannot revoke them unilaterally. It preserves preparation; purchase execution follows immutable accepted terms and current order/financial/security guards, and commercial T remains Subscriptions-owned.

**One decision owner.** Activation and ordinary abort/withdrawal serialize on the same durable Subscriptions operation and preparation-generation fence. The activation transaction verifies the saved commitment and latest locally known evidence, and atomically records applied, T and its publication intent. The competing non-activation transaction irreversibly closes that generation for activation and records a terminal resolution plus withdrawal publication. If activation won, cancellation is a subsequent lifecycle operation against the applied subscription; it cannot be reported as an unapplied acquisition.

Replacing a preparation manifest must first fence the old generation locally and retain its non-activation resolution; a newer revision alone is not proof that old external effects are gone. No remote call occurs under the local commit lock.

**Resolution and recovery.** The applied or withdrawal `CommercialPostureV1` carries an immutable `activationResolution`. Both outcomes identify the authenticated ordering namespace, scope, activation operation, preparation revision/digest and durable decision version. `applied` additionally requires the exact manifest identity/version/digest, readiness commitment binding and T. `closed_without_activation` records the reason and irreversibly fences that preparation generation even when no manifest or commitment exists or has reached Subscriptions. It includes any bindings already known to Subscriptions; missing readiness metadata does not prevent local closure or require a call to Entitlements under the decision lock.

Entitlements verifies the generation and any supplied bindings against its retained preparation input. It associates the closure with all effects of that generation, including a manifest or commitment discovered after closure, without changing the immutable decision or inventing an empty ready manifest. A closure received before preparation is retained as a generation fence: late preparation cannot dispatch new work or publish ready. Already in-flight effects remain subject to provider fencing, recovery and authorized cleanup.

Closure of an older generation never closes a successor; every generation's result remains independently recoverable by ordering namespace, operation and preparation revision, with scope authorization and digest verification.

Later postures retain the resolution reference; the immutable operation recovery read supplies the exact result if events are lost or superseded. On applied, Entitlements continues the same prepared assignments, subject to any already accepted higher lifecycle revision. Recovery of historical activation cannot restore rights after a newer restriction or cancellation. On a proved non-activation fence it settles in-flight work and performs only authorized cleanup. Pending or unreachable results, an expired cursor, a missing row or a failed observation do not prove a terminal outcome.

Entitlements retains outstanding preparation obligations and retries the authorized outcome read; timeout triggers escalation, never cleanup or a second provider selection. A failed execution guard prevents commit but does not release preparation until Subscriptions records the non-activation fence. Unknown capture and financial-remedy obligations follow their separate recovery rules.

**Preparation deadline and attempt outcome.** The service profile defines a versioned preparation-duration policy. At the durable authorized start of an activation attempt, Subscriptions snapshots the selected policy/version, start and evaluated deadline with the operation and preparation posture. Waiting for a requested future start is not preparation time. Dispatch retries, polling, restart and later profile edits cannot move this attempt's deadline. This bounds the technical attempt. It does not expire the accepted purchase, payment, credit commitment or protected capacity claim. Explicit purchased start conditions remain separate.

The local activation decision checks that deadline and the other guards under the supported clock/runtime profile. After it expires, that attempt cannot newly activate even if the timeout worker has not run or a late ready observation reports an earlier timestamp. The deadline worker requests/executes closure at the same Subscriptions operation/preparation-generation fence as activation. The saved profile distinguishes retryable observations from definitive preparation failure. Retryable/unknown results recover the same attempt within its deadline; only a definitive failure uses the same fenced closure path.

If activation already won, recover its applied T and result; later polling or timeout cannot rewrite it as failed or unapplied. Otherwise Subscriptions durably records `closed_without_activation` with the timeout/failure reason and withdrawal outbox. A missing or unreachable outcome, or an uncommitted closure request, remains unknown. It is not a terminal receipt.

Entitlements may request closure and report evidence. Its timer or failed observation cannot prove non-activation. It retains held assignments and recovers the exact resolution. Only a proved closed generation permits authorized cleanup; quota occupancy still waits for confirmed resource release, and trial coverage is settled under its exact commitment. Late ready, allocation or cleanup results remain correlated recovery work. Cleanup and its outstanding effects stay visible independently of the terminal attempt outcome.

Orders handles the closed attempt through remediation while retaining the purchase, accepted terms, protected claim and applicable financial basis. Timeout alone does not cancel the order, release purchased rights, create a refund or incur service-period charges. A successor requires the existing authorized same-acquisition protocol: terminal predecessor non-application, fenced delayed commands, resolved financial applicability and settled or safely quarantined external effects, with generation protection against old cleanup.

It retains the original purchase/payment/benefit lineage, creates no second claim or bonus and does not extend an explicit purchased start limit. Unknown predecessor outcome cannot authorize it. If activation won, continue that applied subscription; stopping it requires its normal guarded lifecycle action. Commercial cancellation and financial remedy remain an explicit separate route. Numeric durations, provider mappings and executed race/recovery evidence remain delivery work.

**Purchased rights and provider restrictions.** A provider safety/technical restriction does not rewrite accepted purchase terms or the commercial desired quantity. Entitlements reports the actual restriction and its authoritative reason/evidence as a fulfillment discrepancy. A known incompatibility for the offered scope must prevent acceptance/capture of that unsupported offer; a restriction discovered after purchase preserves the obligation for remediation. Initial readiness cannot succeed while a restriction prevents a required purchased capability.

Once activation has applied, retain its T/history and handle the reported discrepancy through the existing lifecycle/remediation policy, not a silent reduction of purchased rights. Independently authorized emergency security restrictions remain immediate. Ordinary restriction changes affecting held preparation must honor the existing activation/withdrawal fence; a ready commitment cannot be silently revoked. Provider observations and their recovery do not themselves amend the subscription or create an automatic refund. Concrete restriction values and enforcement stay with the provider.

**Availability and emergency exception.** Readiness observations and the obligation to retain preparation are separate records. A physical service failure is reported truthfully; any known failure or emergency block prevents the local activation decision until resolved. An independently authorized emergency security block may deny access immediately without waiting for Subscriptions, and must publish correlated incident evidence. If its notice races an already applied commit, retain the actual T and follow the existing guarded suspension/cancellation and financial-correction policy; do not backdate or erase activation.

This protocol prevents ordinary control-plane revocation races, not physical outages or instantaneous cross-service knowledge of emergency actions. No new automatic refund policy is implied.

**Preparation exposure and cost.** Preparation requires its action-scoped authority and applicable financial admission before usable access is exposed. Any authorized preparation use before applied T, including a preparation that never activates, is platform-borne and must not become ordinary customer service charges. The enabled profile must bound preparation timing, capabilities, capacity/usage and admission concurrency, with operator escalation and a fenced settlement route. Limits cannot silently clean up an unresolved attempt; a profile unable to reconcile its bounds with held obligations is unavailable.

Required preparation rights must survive unresolved decisions under the provider contract; this is not an extension of purchased validity or trial credit. Actual usage retains its authoritative time and preparation-operation provenance; late arrival does not move it across T. After T, existing paid/trial terms govern. Rating/Billing mapping and boundary evidence must be agreed before the capability is enabled; no zero-price catalog mutation, bonus grant or inferred financial permission is introduced. Quota-dependent and credit-dependent initial profiles retain the compatibility gates below.

**Remaining delivery gates.** Exact SDK encoding, authenticated terminal-resolution reads, provider retention and emergency contracts, preparation bounds, usage classification and the joint race/recovery fixtures remain required. These contracts require integration evidence before the capability is enabled.

**Commit boundary.** After the aggregate manifest result is ready, validate accepted-purchase evidence, its retained financial coverage and explicit disposition fences and recheck local operation/association revision, the saved attempt preparation deadline, order/attempt and cancellation fences, accepted terms, security/Policy and occupancy. Validate the saved held commitment and serialize activation against the terminal non-activation fence under SUB-D-57; known failed/emergency evidence blocks application. The commitment does not replace the other final guards.

Atomically commit active status, own-reservation conversion, fresh local `serviceActivatedAt` T, period start, operation applied result, commercial records and outbox. Do not backdate T to assignment creation, provider `observedAt`, capture or callback time. Publish applied outcome to Orders Workflow from durable state/outbox; transport receipt, a single ready event or an intermediate approved operation is not completion. The initial activation's service-period/accrual boundary is T; separately agreed one-time occurrences retain their existing rules. Prepayment may precede T under existing protected-capacity/terms/capture rules.

All rights essential for initial usable service must be ready before this commit. The post-commit outbox may reconcile the same satisfied assignment or deliver nonessential/ongoing changes; it cannot be the first action that makes required initial access usable. Initial readiness is not an uptime guarantee, and later service failure does not reopen or backdate the completed acquisition.

Entitlements owns manifest construction, trusted producer selection, provider uncertainty handling and cleanup; Subscriptions enforces the consumer contract above.

**Trial coverage and readiness.** A credit trial requires a durable Billing commitment for one reserved grant, exact accepted scope/terms and the saved activation operation/preparation generation. Subscriptions carries its authenticated reference and digest as `trialCoverage` in preparation posture; it is covered by the posture digest and held-readiness binding. Entitlements verifies the commitment and the selected profile's ability to prepare and retain usable access without waiting for later Billing recording.

Successful activation makes that same grant effective at T under Billing's prior guarantee; finalization records the result without a new decision or access gate. Authorized pre-T use is platform-borne and cannot draw down the bonus. Only fenced non-activation permits release, while applied recovery preserves T, original expiry and newer cancellation. No grant is effective merely because preparation or a ready event exists.

Exact S-U15 provider/recovery and bounded enforcement evidence still gate deployment; an adapter that waits for post-T issuance to make essential access usable cannot implement this profile. Free-period trials need no promotional-grant service. The allocation-preparation and consumable-limit readiness contracts below also apply.

**Allocation quota preparation.** An authorized activation operation may prepare its purchased environment while draft using the frozen accepted allocation cap and authorized resource scope. This does not permit unrelated customer allocations or preparation before its allowed schedule. Entitlements owns derivation and the required QE/adapter contract; Subscriptions publishes only commercial preparation intent and consumes aggregate readiness. QE counts actual occupied/reserved capacity immediately.

Activation continues the same quota source/member, resource and allocation identities without a counter reset, second allowance or access gap; a delayed applied-posture delivery must not interrupt held prepared rights. For an illustrative 100 GiB cap and a prepared 20 GiB disk, occupancy remains 20 GiB and available capacity 80 GiB after T. Pre-T cost is platform-borne but occupancy is real. Only exact fenced non-activation permits withdrawal: block new allocations and clean up only authorized resources, releasing counted capacity only on confirmed resource release.

Unknown outcomes or cleanup timers cannot free it. Quota permission is not proof of physical capacity; actual prepared service must satisfy readiness. Consumable-limit readiness follows the provider-check purpose/evidence contract below; execution and quota treatment remain provider-owned. Unsupported provider capability refuses the profile before sale/capture; temporary outages preserve accepted purchase obligations. Quotas explicitly outside initial readiness may retain post-T delivery.

**Consumable-limit readiness.** Preparation configures the accepted limit, authorized scope, window and enforcement/accounting path and obtains authoritative provider readiness evidence. Entitlements marks requests made to check readiness with a purpose marker, semantically `purpose = readiness_check`, in the scoped adapter request context. The marker lets the adapter/provider recognize the request; execution method and quota/accounting treatment belong to that provider's contract. It does not itself promise dry-run behavior, no side effects or zero consumption, and grants no authorization, quota or billing exemption.

No consuming probe or service allowance is universally required. Ready still needs trustworthy evidence for the exact assignment/revision; the marker alone and queued configuration are insufficient. Activation/recovery retain the same rule/counter/window without reset or another allowance; QE owns window progression. Any actual effects remain visible and recoverable under the provider's declared handling and the existing platform-borne pre-T cost rule. Deferred runtime scope is unchanged.

### Apply a commercial posture and record its observations

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-flow-entitlement-posture`

For initial activation, apply IA-2026-10-18 above first: required preparation and usable access precede commercial application. The following procedure describes ongoing posture changes and nonessential post-commit reconciliation; it must not postpone an initial prerequisite until after active.

1. The lifecycle handler derives the complete target commercial grant/posture set from immutable accepted product descriptors and committed quantity at the boundary.
2. The Foundation commit stores desired posture, effective interval, source aggregate revision, tenant axes and the outbox intent atomically with the commercial change.
3. Publish `CommercialPostureV1` to Entitlements through the outbox, identified by `(orderingTenantId, subscriptionId, revision)` and the payload digest, carrying the operation id and provenance (acceptance id, rule/binding snapshot digests, contributing lot ids). Recovery reads expose the same identity and digest, so a replay or a late reader obtains the same revision. A newer revision supersedes the target; earlier outcomes are retained.
4. Record the readiness and materialization observations Entitlements reports against the matching revision only, and surface them through subscription reads: desired revision, latest observed state, last observed time and findings. Show pending or failed delivery explicitly instead of reporting implied access.

**Execution context in `CommercialPostureV1`.** Every payload carries a `lifecycleAction` (`prepare_activation`, `apply_lifecycle`, `withdraw_preparation` or `update_disposition`) and, for an applied lifecycle change, its transition kind and effective boundary. Preparation and withdrawal use the same durable activation operation; every changed intent gets a new aggregate revision and digest even while status remains draft. Withdrawal is persisted with the local activation fence before publication; it targets the previous preparation revision. No new commercial status or pre-T service start follows.

**Resource disposition in `CommercialPostureV1`.** The complete `resourceDispositions` collection carries each retained disposition's `dispositionId`, logical component/resource binding and provider-independent immutable resource generation reference, confirmed charging-cutoff evidence where present, frozen retention/export terms and policy digest, deletion schedule generation and future deadline, hold policy version and current desired hold state, prerequisite/notice evidence references, and explicit `deleteAuthorized` eligibility. These are commercial instructions and authoritative references, not vendor commands. Entitlements resolves the binding through its retained assignment mapping; ambiguous or unknown generations refuse destructive work.

Absent confirmation is not a cutoff, absent notice is not delivery, and absence of a disposition is not permission to delete.

Under `update_disposition`, hold placement, release, rescheduling, cutoff confirmation and deletion authorization advance the same aggregate revision and atomically publish the complete collection even if all members remain `Revoked`. Subscriptions serializes them against its local eligibility decision; Entitlements orders them against the resource effect owner. A newer hold clears `deleteAuthorized`; final hold release creates a new schedule generation and future deadline, with fresh required notice evidence before authorization. Reordered older snapshots cannot restore deletion permission.

Withdrawal/removal of a disposition is explicit and retains its fence and historical outcomes. Recovery reads return the same revision/digest and collection; actual stop/delete and confirmed hold protection return through Entitlements' authorized materialization reads. A requested hold is never reported protected before execution-boundary confirmation.

## 3. Processes / Business Logic (CDSL)

### Posture by transition

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-entitlement-posture`

| Commercial event | Desired posture and boundary |
| --- | --- |
| Initial activation | Required assignments are prepared and confirmed usable while draft; guarded local application records the accepted active target and actual T without issuing initial prerequisites for the first time afterward. Import/resource adoption is deferred. |
| Composition/quantity change | New seat purchases/increases/additions require an admitted active target (draft changes retain the separate S-U01 gate); suspended requests refuse before commercial effects. For an applied active change, replace the complete target at its committed boundary; no transient union of old/new grants. |
| Suspend | Restricted/paused posture preserving data and restoration identity. |
| Resume | Restore the accepted set only after required lifecycle/occupancy guards. |
| Cancel | Revoke normal service, with separate narrow export capability during permitted retention. |
| Trial conversion | Target paid posture only after S-Q06 model and the applicable authorized settlement route: prescribed payment for prepay, or accepted postpay terms plus authorization to serve on credit. A linked card or immediate payment is not required for authorized postpay. |

**Purchase-state rule:** require a separate authorized guarded resume before purchasing seats, increasing quantity or adding components to an existing suspended subscription (SUB-D-39). The purchase itself never resumes or queues a deferred grant. This is compatible with replacement integrations that require an active/usable successor, while keeping the core provider-independent and introducing no new synchronous readiness gate.

If an active purchase already committed before suspension, preserve its commercial history and reconcile its pending delivery under the newer restricted revision; the provider fence must prevent stale replacement/issue from restoring access. Renewal recovery and unrelated administration retain their own guards.

Where the product requires no access gap, Entitlements and its selected adapter must demonstrate that continuity guarantee; its replacement or readiness mechanism belongs to the integration specification. Subscriptions cannot promise issue-before-revoke continuity by sending two asynchronous commands. IA-2026-10-18 makes initial usable readiness mandatory before initial commercial activation, subject to the joint freshness/withdrawal contract. It does not add a synchronous gate to resume or active changes. Circular waiting for a post-activation command is prohibited.

### Financial boundary of posture

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-posture-financial-boundary`

Counting, seats and point-of-use decisions belong to the providers behind Entitlements. Prepaid wallet balance/drawdown remains Billing/Rating-owned and launch-gated until that separate contract is accepted. Promotional trial credit is distinct from paid wallets: Billing owns its grant/balance/allocation/correction direction, while exact provider/Ledger integration remains OPEN under S-U15. Early valid guarded paid conversion closes the trial episode while retaining unused grant eligibility for paid expenses until original expiry under unchanged restrictions. It grants no access by balance alone and neither renews trial access nor issues a new grant.

A financial correction cannot itself restore access or reopen a closed trial. After grant expiry, corrected credit remains unavailable to spend and retains the original expiration; no trial extension follows. A later correction cannot automatically reopen a trial already ended by exhaustion, even before original expiry: restored accounting balance grants no service/spend permission for that closed episode and does not reverse an applied paid conversion. V1 has no reactivation route; new acquisition is a fresh purchase; repeat trials are deferred.

Platform liability for eligible accepted-trial value beyond bounded bonus, including positive corrections and delay/error overrun, served before the applied paid boundary is accepted; post-boundary expenses follow accepted paid terms. Financial protection is not a cap applied by Entitlements or at the point of use; asynchronous usage/Rating/access effects can overshoot, so credit launch needs explicit coverage and enforcement contracts. Subscriptions calculates neither remaining balance nor quota, and unknown credit evidence is not spend permission.

## 4. States (CDSL)

### Entitlement delivery state

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-state-entitlement-handoff`

Subscriptions records, per desired revision, the delivery and materialization states Entitlements reports (pending/acknowledged/failed/withdrawn; pending/applied/failed). These are observations reported by Entitlements, not subscription statuses and not a Subscriptions state machine. A later desired revision supersedes the target but retains earlier outcomes. Failed handoff never silently broadens commercial access.

## 5. Definitions of Done

### Commercial posture publication and observation intake

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-dod-entitlements`

Implement atomic posture/outbox persistence, posture publication, observation intake and the read surface.

**Implements:** `cpt-cf-bss-subscriptions-flow-entitlement-posture`, `cpt-cf-bss-subscriptions-algo-entitlement-posture`, `cpt-cf-bss-subscriptions-algo-posture-financial-boundary`, `cpt-cf-bss-subscriptions-state-entitlement-handoff`.

## 6. Acceptance Criteria

- [ ] A provider hard ceiling below the purchased cap remains an explicit fulfillment discrepancy, not an amended purchase or successful full delivery. Known offer incompatibility prevents sale; later restrictions retain the obligation. Required initial capability cannot be ready under the mismatch; held-readiness and emergency races preserve the exact activation outcome.

- [ ] Readiness-check requests carry a recognizable purpose in the exact scoped adapter context. Provider evidence proves readiness; labeling alone does not imply dry-run, zero consumption, privilege or success. Actual effects follow provider handling and recovery; activation/retry preserve rule/counter/window.

- [ ] Preparation deadline is snapshotted at authorized attempt start and survives retries/restart/profile edits. Expiry or definitive failure closes only that attempt through the Subscriptions fence; late readiness cannot activate the closed/expired attempt. A prior applied winner retains its original T.
- [ ] Lost closure results retain held preparation and recover by the same identity. Timeout alone creates no refund or purchase cancellation; successor admission requires predecessor non-application and safe external-effect/cleanup fencing while retaining original financial and commercial lineage.

- [ ] Concurrent activations in one scope receive different operation-correlated manifest results; a fast ready, duplicate, conflicting roster, stale sequence and lost event recover only their saved operation. An authoritatively empty manifest produces explicit ready evidence.
- [ ] A hold or release while the subscription remains revoked publishes a newer posture with the complete disposition state. Reordering cannot authorize a stale deletion; unknown protection remains pending.
- [ ] Allocation-dependent initial readiness uses authorized preparation within the accepted cap and exact resource scope. Occupied capacity survives T under the same source/member/allocation identities, including delayed posture delivery; no double allowance or reset. Unknown activation retains capacity; fenced abort blocks new allocation and only confirmed resource release frees occupancy. Check-request labeling alone cannot prove readiness or alter provider quota treatment.
- [ ] A crash after commercial commit and before dispatch recovers the same posture publication without losing posture or duplicating materialization.
- [ ] The read surface distinguishes commercially active, delivery pending, materialization failed and observed success without asserting point-of-use access.
- [ ] Suspend preserves resource data and restoration identity; cancel grants only the explicitly accepted export capability. New suspended seat purchases/increases/additions produce no desired purchase grant; a separate guarded resume precedes normal active change. A committed-before-suspend purchase remains recoverable, and its late delivery cannot override the newer restriction.
- [ ] Authorized postpay conversion without a linked card or immediate payment produces the accepted paid posture once; absent required credit-service authorization or unsatisfied prescribed prepayment cannot produce it. Entitlements reports materialization asynchronously, and Subscriptions records it separately from commercial status.
- [ ] A credit trial prepares usable access under its exact Billing coverage commitment; local activation makes the same grant effective at T. Delayed/lost Billing finalization does not delay initial access, reset expiry or create another grant. Pre-T use never consumes the bonus. Cancellation before T releases only after the non-activation fence; unknown outcomes retain protection, and late applied recovery cannot revive later cancelled access.

- [ ] Timeout closes an attempt before Entitlements receives preparation or before Subscriptions receives any manifest result. The generation-bound resolution is recoverable without a readiness commitment; late preparation and ready events cannot reopen it.
- [ ] One required resource is prepared and another fails before aggregate ready. Terminal non-activation closes that generation without fabricating a commitment; Entitlements recovers partial effects and completes only authorized cleanup. A later successor and its resources remain protected from the old cleanup.

- [ ] Activation and withdrawal race on one operation/generation: exactly one terminal outcome wins. Lost applied publication recovers the same T and keeps prepared rights; lost withdrawal recovers a durable non-activation fence before cleanup.
- [ ] A timeout, failed observation, an unmet accepted start condition or missing recovery row never independently releases a readiness commitment. A superseded manifest cannot later activate, and its retained resolution permits only its own authorized cleanup.
- [ ] An emergency block denies access immediately, records its actual time and is reconciled against the durable activation outcome; a late notice never erases an applied T.
- [ ] Authorized preparation use before T, including never-activated work and late-arriving usage, creates no ordinary customer service charge; post-T usage keeps the accepted paid/trial treatment. Unsupported exposure bounds or accounting evidence prevent profile enablement.
