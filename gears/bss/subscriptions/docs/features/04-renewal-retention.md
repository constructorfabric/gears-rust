---
description: "Renewal, grace, cancellation, retention and offboarding with frozen terms and recoverable provider outcomes."
---

# Feature: Renewal, Grace, Suspension and Retention

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Renew once, or enter an evaluated grace episode](#renew-once-or-enter-an-evaluated-grace-episode)
  - [Suspend and resume existing resources](#suspend-and-resume-existing-resources)
  - [Collection pause (deferred)](#collection-pause-deferred)
  - [Cancel, retain and delete](#cancel-retain-and-delete)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Evaluate term and co-termination policy](#evaluate-term-and-co-termination-policy)
  - [Grace, dwell and dispute clocks](#grace-dwell-and-dispute-clocks)
  - [Notices and scheduled precedence](#notices-and-scheduled-precedence)
- [4. States (CDSL)](#4-states-cdsl)
  - [Renewal and retention state](#renewal-and-retention-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Governed renewal and confirmed offboarding](#51-governed-renewal-and-confirmed-offboarding)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Renewal and offboarding integration contract](#7-renewal-and-offboarding-integration-contract)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-featstatus-renewal-retention-implemented`
- [ ] `p1` - `cpt-cf-bss-subscriptions-feature-renewal-retention`

**Status:** target design. Retained occupancy, confirmed cancellation cutoff, cause-specific stop treatment and frozen versioned policies are specified. S-U05/S-U13 owner protocols, S-U10 occupancy integration and measured S-Q07 evidence remain release gates.
## 1. Feature Context

### 1.1 Overview

Execute accepted term policy while keeping service access, obligation production, collection delay and data retention distinct and recoverable.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-subscriptions-fr-suspend-resume`, `cpt-cf-bss-subscriptions-fr-suspension-billing-posture`, `cpt-cf-bss-subscriptions-fr-renewal-evaluation`, `cpt-cf-bss-subscriptions-fr-renewal-auto-manual`, `cpt-cf-bss-subscriptions-fr-failed-renewal-ladder`, `cpt-cf-bss-subscriptions-fr-grace-policy`, `cpt-cf-bss-subscriptions-fr-resource-disposition`, `cpt-cf-bss-subscriptions-fr-record-retention`.

**Deferred references, not current requirements:** `cpt-cf-bss-subscriptions-fr-collection-pause`, `cpt-cf-bss-subscriptions-fr-renewal-notices`.

**Principle:** `cpt-cf-bss-subscriptions-principle-when-not-math`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-subscriptions-actor-partner-admin` | Requests suspend/resume, renewal and cancellation; collection pause is deferred. |
| `cpt-cf-bss-subscriptions-actor-contracts` | Supplies explicitly bound terms and grace overrides; notice implementation is deferred. |
| `cpt-cf-bss-subscriptions-actor-payments` | Supplies payment results, retry exhaustion and dispute status. |
| `cpt-cf-bss-subscriptions-actor-oss-provisioning` | Confirms pause, restoration, retention and deletion effects. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [domain and stores](../DESIGN.md#31-domain-model).
- **Build prerequisites:** features [01](01-foundation.md) and [02](02-composition.md), matching DECOMPOSITION.
- **Integration acceptance:** [05](05-entitlements.md) materializes posture, [08](08-commercial-facts.md) consumes accepted period geometry, and [09](09-consumer-contracts.md) supplies provider/recovery adapters. These are later end-to-end conformance requirements, not reverse build dependencies. Enable each renewal/retention capability only after its S-Q05/S-Q07 and S-U05/S-U06/S-U07/S-U11/S-U13 prerequisites and applicable materialization contract are accepted.
- **Source decisions:** [DECISIONS.md](../DECISIONS.md), SUB-D-03/12/13/16/17/20/22/23/29.

## 2. Actor Flows (CDSL)

### Renew once, or enter an evaluated grace episode

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-flow-renewal-grace-rnw`

1. At the accepted term boundary, acquire the `(subscriptionId, currentTermSequence)` renewal fence and inspect pending non-renewal/cancel intent first. Its suppression wins over automatic and manual extension until explicitly resolved.
2. Evaluate explicitly bound Contract terms or versioned platform defaults and retain the selected provenance. Accepted order-line terms determine a new uncontracted subscription's fixed/evergreen term; ContractSigned alone never creates or binds a subscription.
3. If auto-renew is disabled and no manual renew exists, execute end-of-term cancel with reason `term_expired`. Evergreen end-of-term means the end of the current commercial billing period.
4. Stage the settlement evidence required by accepted terms (prescribed prepay payment or authorized postpay eligibility) and supported renewal bindings. Recheck current local state at application. Collection-pause bypass is deferred.
5. Successful scheme-specific admission extends the term exactly once, freezes renewed terms and opens due commercial facts. Definitive financial refusal follows the evaluated grace policy and preserves blocked next-term obligation identities; unknown/dependency failure remains recoverable. Notification execution is deferred.
6. During grace do not emit a blocked next-term recurring obligation before a successful renewal. Preserve actual service/usage and independently valid usage facts under accepted policy.
7. Late success completes that same renewal identity with a term starting at its original boundary. Freeze historical composition, payer and applicable accepted terms for the described range and retain actual suspension/service intervals; renewal continuity does not backdate actual service activation or fabricate service.
8. Grace exhaustion follows its evaluated ladder, typically suspend then bounded nonpayment dwell/cancel. Payment retry exhaustion can trigger the same exit before the time deadline. Dunning/PSP execution belongs to Billing/Payments.

### Suspend and resume existing resources

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-flow-suspension-retention`

1. Validate Policy and source status; commit the restricted posture for the boundary; Entitlements issues the idempotent pause work order.
2. Suspend data-bearing resources without deletion. In the applied suspension transaction convert the same active occupancy claim to a retained suspended place, without reducing total claimed capacity. Persist the restriction intent and evaluated suspension billing policy: pause recurring or continue only as expressly accepted. Record actual pause/outcome evidence in a later revision-correlated observation transaction after Entitlements reports it; pending or failed pause never masquerades as completed execution. Keeping the place grants no billing waiver or usable access.
3. Resume still requires applicable payment, Policy, term and resource guards; Entitlements materialization remains separately observable. On an unchanged retained claim, atomically convert that place to active without competing for or acquiring a second place. If a renewal was blocked, complete that renewal before service resumes; if none was blocked, do not invent one. Retained occupancy is not a guarantee that these other requirements will succeed. A reduced versioned limit does not evict this retained claim or make unchanged-key resume compete for another place.

   Policy/key movement still requires the S-Q02/S-U10 destination/version mapping and atomic move; unsupported or failed movement preserves the old claim and refuses.
4. Payment resolution voids a nonpayment-dwell cancellation deadline even if Policy or another independent guard prevents resume. A paid customer cannot be cancelled by a stale debt timer; its retained claim is not released merely because resume remains blocked.
5. Resume targets the same retained resources and re-arms applicable parked lifecycle intents. Deleted resources cannot be resurrected by status change; collection-window expiry is outside the current profile.

### Collection pause (deferred)

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-flow-collection-pause`

**Deferred.** No collection-pause window, pauseCollection/resumeCollection operation or renewal bypass is implemented.

### Cancel, retain and delete

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-flow-offboarding`

1. Resolve accepted cancel mode/reason, any explicitly customer-confirmed charging cutoff, actual service-stop effect and commercial boundary through the Foundation. Financial confirmation is distinct from request receipt and eventual guarded cancel completion. Customer cancellation is confirmed only after authorization, commercial eligibility and durable recording of its charging cutoff; confirmation is independent of resource provider completion. Withdrawal or rescheduling requires an explicit versioned confirmed action before the applicable cutoff, without retroactively restoring charges.

   After cutoff, new service needs a separate valid guarded route, not undo of the financial commitment. At the applied cancellation commit, release the active or retained suspended occupancy claim and end normal entitlement; create an independent retention disposition for data-bearing resources. Pending, scheduled or failed cancellation retains the claim. Resource retention or legal hold after applied cancellation does not retain normal subscription occupancy.
2. Evaluate accepted retention policy and freeze its deadline/provenance; the accepted default retention duration remains 30 days with read-only export. Notification scheduling is deferred. If accepted terms require notice before deletion, keep deletion pending rather than treating absent delivery as completed.
3. Apply accepted S-Q05: ordinary customer tariff ends at the explicitly confirmed cancellation charging cutoff despite platform technical delay, and cannot continue after authoritative cancellation/offboarding stop. A future scheduled cutoff allows normal service under existing terms before then; request receipt alone establishes no cutoff. Preserve authoritative runtime history and retain its relevant evidence/lineage plus the nonchargeable technical extent under the frozen policy/owner contract. This does not alter suspension bill/waive policy or transient-outage treatment.

   Keeping an offboarded server's disk for recovery/export does not continue the former server tariff. Resource presence, export, hold or delayed deletion alone creates no storage fee. Explicit agreed retention terms, including existing terms, may support their own retained-storage obligation; a new purchase is not universally required.
4. Before deletion, check current subscription/payer legal holds and the current deletion-schedule generation under a serialized deletion fence. The resource provider must honor a corresponding hold/cancellation protocol; a cached negative hold read is insufficient permission to delete after a hold arrives. Hold placement invalidates queued eligibility from the previous schedule without erasing its audit.
5. When the final applicable hold is released, persist authorized release and a fresh evaluated deletion schedule with a new generation, policy provenance, release instant and future deadline. The old expired deadline cannot authorize immediate deletion. If bound terms require notice, preserve that prerequisite and keep deletion blocked while notice delivery is unavailable; no notification scheduler or default 7/1-day delivery task is implemented here. Prior notice cannot prove notice of a newly scheduled deletion. Unsupported schedule/policy remains visible as a finding.
6. Publish the complete resource disposition in a newer `CommercialPostureV1`, as defined in [feature 05](05-entitlements.md#apply-a-commercial-posture-and-record-its-observations). Hold placement, release and rescheduling also publish newer revisions while commercial status remains unchanged. Release the deletion schedule generation to Entitlements only when the current schedule is eligible, required notice conditions are met and no applicable hold remains; Entitlements issues the idempotent delete work order and Subscriptions retains its confirmed outcome. A stale worker carrying an earlier schedule generation cannot delete after release/rescheduling. A timeout is unknown deletion, not completion.

   Hold-vs-delete and release-vs-stale-worker races require a jointly accepted resource provider fence; until then automatic destructive deletion is not enabled.
7. Keep original usage coordinates, attribution, frozen terms and version lineage for late pre-cutoff usage/corrections, including after deletion. Arrival time cannot move usage across the cutoff. Rating values the obligation; Billing corrects erroneous technical-tail charges/postings and settles resulting overpayment idempotently. This is correction, not an additional goodwill payout or a selected cash-refund method/timing/credit formula. Legitimate prior debt, commitments and explicit retention obligations survive; erroneous tail charges are not protected as prior debt. Financial evidence retention remains unchanged.
8. Archive only after the configured cancel dwell (90 days is the accepted v1 default) and completion of the current data-window/delete disposition, including any replacement schedule after hold release. Purge financial evidence only after the applicable configured contractual/legal retention period, never under an active hold. Personal-data erasure tombstones references while preserving required invoice reproduction evidence.

Authoritative correlated stop evidence distinguishes actual effective cancellation/offboarding stop from callback arrival; a later scheduled cutoff cannot extend ordinary accrual after that stop. Unknown stop remains operational uncertainty and can leave final closeout pending, but cannot erase a known confirmed charging cutoff or extend billing beyond it. A worker recovering a confirmed 10:00 cutoff at 10:05 preserves that financial boundary and truthful later stop/state history; it does not fabricate a 10:00 physical stop or grant general backdating permission. The example is not an SLO.

S-U05/S-U13 must define versioned financial confirmation separately from eventual commercial/resource completion and accept evidence, capture/compensation/state and correction fences. Internal stops are split by cause: a platform-fault or maintenance stop beyond the accepted SLA ends ordinary accrual at the authoritative stop and Billing corrects any charged tail as erroneous, with SLA credits/remedies outside Subscriptions; an operator, abuse or policy stop follows the existing suspension bill/waive policy, creates no charging cutoff by itself, and cancellation still needs the normal confirmed route.

Exact protocol remains OPEN; confirmed withdrawal/rescheduling follows the pre-cutoff versioned-action guard above. A cutoff alone permits no early claim release, deletion or archive. Hold/notice safeguards remain unchanged and numeric examples remain illustrative; collection pause is not a billing waiver.

An unactivated confirmed order follows the Foundation's reservation cancellation path: fence consumption and capture, recover any unknown financial outcome and record the agreed remedy obligation. It may be prepaid while the subscription is still draft; no service fact does not mean no financial consequence. A paid future-start reservation survives date waits, holds and ordinary TTL, and technical failure keeps it for remediation or explicit cancellation. Unpaid abandoned-draft cleanup must not apply to paid or payment-uncertain rights.

Refund policy and terminal release/settlement ordering remain owner contracts; this feature adds no automatic refund or universal wait-for-posted-refund rule.

## 3. Processes / Business Logic (CDSL)

### Evaluate term and co-termination policy

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-renewal-terms`

Evaluate the current fixed/evergreen term, billing cycle, auto-renewal and grace/retention inputs from explicitly bound terms or versioned defaults. Freeze the evaluated source versions and deadlines; a new configuration cannot rewrite an existing term. Co-termination/date alignment and notice scheduling are deferred. A future term that requires unavailable notice or acceptance behavior cannot silently skip that prerequisite.

### Grace, dwell and dispute clocks

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-grace-dwell`

Grace duration comes from evaluated versioned policy; 7 calendar days is the accepted v1 platform default; overrides require the accepted Contract/platform bounds. The next-term recurring obligation is blocked until renewal success, never emitted solely on failure. A nonpayment suspension records an evaluated dwell, with 90 days as the accepted v1 default. Config changes do not move an existing deadline. Payment resolution, any committed exit from suspended, or terminal cancellation voids that episode's timer.

A dispute/chargeback hold on the blocking charge stops only the dwell clock and resumes it on resolution; it is not service permission or collection pause. No TTL or dwell expiry silently releases a retained occupancy place. An accepted timeout policy may trigger normal guarded cancellation; the claim persists until that cancellation successfully applies, including during pending or failed cancellation.

Payer/resource-owner transfer is deferred. The existing suspension episode keeps its evaluated payer/term policy and deadline; resolving nonpayment does not silently resume service or bypass current guards.

### Notices and scheduled precedence

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-algo-renewal-notices`

**Deferred.** No renewal/price-change/nonpayment/deletion notification scheduler or delivery adapter is allocated. End-of-term opt-out remains ordinary cancellation; Foundation still orders scheduled commercial intents. A workflow whose accepted terms require a notice stays unavailable until that prerequisite can be met.

## 4. States (CDSL)

### Renewal and retention state

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-state-renewal-retention`

Renewal evaluation is awaiting/succeeded/failed; grace is running/resolved; dwell is running/held/void/expired. These are evaluated episodes over the five subscription statuses. Retention independently records window-open/held/delete-pending/delete-confirmed/archive-eligible; deletion uncertainty is retained as pending with a finding. Releasing the final applicable hold moves retained resources to window-open under a new auditable schedule generation, not directly to delete-pending because the old deadline passed. After restart, no episode is reconstructed from current configuration.

## 5. Definitions of Done

### 5.1 Governed renewal and confirmed offboarding

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-dod-renewal-retention`

Implement term-sequence renewal fencing, evaluated default/Contract policy, cancellation/grace precedence, dwell recovery and required notice-evidence references. Notification scheduling/delivery and collection pause remain deferred. Offboarding must persist actual resource outcomes, protect holds against deletion races, replace deletion eligibility with a fresh versioned schedule and required prerequisite evidence after final hold release and preserve historical attribution independently of operational data. S-U05 must accept the release scheduling/notice policy and schedule-generation fence alongside the existing hold protocol.

Release requires contracts/evidence for accepted post-stop and confirmed charging-cutoff boundaries, remaining confirmation/cancellation cases, financial correction and destructive-action fencing, plus applicable NFR budgets.

**Implements:** `cpt-cf-bss-subscriptions-flow-renewal-grace-rnw`, `cpt-cf-bss-subscriptions-flow-offboarding`.

## 6. Acceptance Criteria

- [ ] Concurrent auto/manual renewal and restart extend a term once; a pending cancellation prevents both.
- [ ] Late payment resumes the correct blocked renewal using historical payer/composition; suspension intervals remain visible to Rating.
- [ ] Payment resolution followed by Policy or another independent resume refusal voids the old debt timer, retains the place and never cancels the paid customer later through that stale timer.
- [ ] Suspended resources remain intact after 89 days; permitted resume restores the same resources and converts the retained unchanged-key place without another slot. A TTL/dwell expiry alone does not free capacity; any timeout cancellation releases only when its guarded commit succeeds.
- [ ] Denied/ineligible cancellation or a failed durable-cutoff write returns no customer confirmation; successful confirmation does not wait for resource provider completion. A versioned withdrawal/reschedule confirmed before cutoff supersedes the prior evidence; a stale worker cannot restore it. At/after cutoff, no unapplied-work shortcut restores charging.
- [ ] A configuration revision cannot move running grace/dwell/retention deadlines; explicitly bound terms take precedence and later evaluations retain their selected version. The v1 platform defaults apply when no bound term overrides them.
- [ ] Confirmed stopped service with delayed deletion/export/legal hold accrues no former service tariff or new storage fee without explicit retention terms. Applicable already-agreed retention terms produce only their own obligation; deletion safeguards remain unchanged.
- [ ] Confirmed cancellation cutoff 10:00 with actual stop at 10:05 creates no ordinary customer tariff for 10:00–10:05, while runtime/platform-cost history remains truthful. A worker recovering at 10:05 preserves the known cutoff without inventing a 10:00 physical stop or applied cancellation.
- [ ] Mere request receipt creates no confirmed charging cutoff. Early confirmation of a future scheduled cutoff allows normal service before that cutoff; an authoritative earlier offboarding stop still ends ordinary accrual.
- [ ] Unknown stop leaves operational/final closeout pending but never extends customer billing beyond a known confirmed cutoff. It proves neither deletion nor entitlement restoration; occupancy releases only at applied cancel, with hold/retention safeguards intact.
- [ ] Delete response loss remains explicit uncertainty. Late pre-cutoff usage/corrections retain original coordinates, attribution and frozen terms; delayed reporting cannot move usage into or out of the nonchargeable technical extent. A late authoritative stop callback does not extend service to its arrival.
- [ ] An already posted/captured technical-tail charge is corrected and overpayment settled through the owner-agreed path without duplicate adjustment on retry. Legitimate prior debt/retention obligations survive; no additional goodwill payment or refund method is inferred.
- [ ] Archiving before both its dwell and data-window completion is refused; evidence purge under a financial hold is refused.

- [ ] A hold outlasting retention is released: create a fresh guarded deletion generation, reject stale workers and keep deletion pending if a bound-term notice prerequisite is unavailable. No notification scheduler is required or falsely reported complete.

- [ ] A suspension commits restriction intent and retained occupancy before the pause result exists; a later failed or successful pause updates only correlated execution observations and never fabricates physical completion.

## 7. Renewal and offboarding integration contract

**RO-2026-10-15: S-U05/S-U06/S-U07/S-U13.** This specifies the retained term, cancellation and disposition protocol and mirrors the boundary in Contracts and the Rating/Billing handoff. Resource execution, entitlement materialization and inventory/evidence remain abstract provider roles: a specific license/resource provider is not mandatory. Contracts in this checkout has a PRD/metadata but no delivered provider SDK/implementation; selected term and resource adapters must demonstrate the contract. This package adds no notification delivery, collection pause, co-term, migration/import, resource transfer, general plan change or new subscription status.

**RO01 evaluated terms and owner boundaries**

| Record | Required evidence and authority |
| --- | --- |
| Evaluated term | Subscription/current term sequence; explicit Contract binding/version where present; accepted acquisition/renewal terms; selected versioned defaults; effective scope and term start/end, cycle/anchor, auto-renew election, supported grace and bill/waive rules; required assent/notice conditions and provenance. Subscriptions freezes the evaluation, not merely a pointer to current configuration. |
| Term-provider answer | Authorized seller/payer/subscription/binding scope, immutable source version/digest and effective interval; supported term fields, applicability and authoritative missing/expired/successor disposition. Outage, denied scope or unknown applicability is not absence and cannot select defaults. Mere Contract presence/signature creates or binds nothing. |
| Renewal episode | Stable subscription/term-sequence occurrence, source term/evaluation, attempt/outcome and FA permission references, blocked next-term fact identities, grace deadline, cancellation suppression and local applied result/outbox. Transport retries and auto/manual triggers share that occurrence. |
| Suspension/dwell episode | Applied suspension and cause; original resources/retained claim; explicit billing posture; exact blocking obligation/outcome versions; evaluated dwell/remaining duration and dispute intervals. A generic payment success is not resolution of this episode's debt. |
| Cancellation confirmation | Authorized request/business identity, applicable accepted eligibility/terms, intent and confirmation revision/digest, mode/reason and effective charging cutoff, state of withdrawal/supersession, durable receipt and publication/recovery work. Request receipt or accepted asynchronous work alone is not this confirmation. |
| Provider effect | Subscription/resource scope, operation and desired-state/disposition generation, requested action/effective boundary, idempotency identity, authoritative accepted/applied/unapplied/unknown outcome and actual effective time distinct from observation/receipt time. Positive correlated evidence alone establishes a resource stop/delete. |
| Retention and hold | Applied-cancel origin; retained resource identities/generation, evaluated duration/deadline/export policy and anchor, applicable hold scopes/versions, required notice evidence, current deletion schedule/generation and resource-owner execution fence; archive and financial-evidence retention remain separate. |

Explicitly bound applicable terms take precedence over versioned defaults; initial uncontracted term shape comes from accepted order terms. Freeze every running term/grace/dwell/retention evaluation. A Contract amendment/expiry event does not mutate it, implicitly bind a successor or immediately cancel service. At next renewal select the explicitly bound applicable successor, otherwise the accepted versioned-default route. That route still requires any applicable price-change notice/assent; an unavailable prerequisite blocks it rather than silently preserving or replacing a price. A bound provider outage does not prove expiry/no successor.

Immediate rebinding is not implicitly enabled by this package.

The v1 defaults are: grace 7 calendar days, nonpayment suspension dwell 90 days, retention 30 days with read-only export, and archive eligibility after 90 days of cancel dwell plus completed disposition. UTC/calendar arithmetic and the anchor are explicit in the evaluated record; no new duration is selected. Notice delivery remains deferred. If the selected terms/defaults require notice, its absence remains an unsatisfied prerequisite; this package neither waives it nor implements a scheduler. Any supplied evidence must bind the exact operation, terms and schedule generation.

Prior notice does not establish a new rescheduled deletion notice.

**RO02 renewal, grace and late success**

Use one local renewal fence per subscription/current term sequence. Before staging financial/price work and again at local commit, check current aggregate revision, cancellation/opt-out suppression, applied term sequence and supported source state. Automatic and manual renewal compete for the same occurrence. Obtain immutable terms/bindings and [FA financial authority](../DESIGN.md#3311-financial-admission-integration-contract) outside the transaction; local application records the next term, source references, full affected facts/seal, receipt and outbox atomically. A stale worker cannot renew a superseded or terminal subscription.

Config changes and transport retries do not create a new occurrence or move its evaluated deadline.

Definitive financial refusal follows the frozen grace policy; unknown outcomes preserve recovery and do not authorize renewal. Grace starts at its accepted term boundary and does not restart on each failed retry. A blocked next-term recurring obligation is not emitted merely because grace began. Actual permitted service/usage and valid independent usage facts remain truthful under the accepted grace/billing posture. Collection-pause bypass and trial-grace inheritance are unavailable.

If that same renewal later succeeds while still admissible, its term retains the original boundary; the receipt also records actual application time and the genuine served/suspended intervals. Rating receives those intervals and frozen terms, not fabricated continuous service. Any liability follows accepted historical service/posture; the callback cannot invent waiting-gap debt or backdate activation/resume. This continuity rule is distinct from a new trial-to-paid start.

Multiple elapsed boundaries cannot be skipped by one unqualified extension: each supported term occurrence needs its own evidence; unsupported historical catch-up stays pending for explicit recovery. An already cancelled subscription does not reopen because a payment arrives.

An absent manual renewal with auto-renew disabled follows the accepted end-of-term cancellation route. A scheduler deadline is not proof of actual stop, applied cancellation or financial settlement; the route must durably derive/confirm its financial boundary from the accepted term/cancel rules. If a cancellation/non-renewal intent fails or remains uncertain, it continues suppressing renewal until explicitly resolved, as Foundation requires.

**RO03 dwell, dispute and resume**

A nonpayment dwell timer binds the exact suspension episode and blocking obligation/version. Authoritative resolution voids that timer even if Policy, term or resource guards still prevent resume. A late unpaid/retry-exhausted event older than the resolved head cannot recreate it. A dispute hold pauses only the applicable dwell countdown; persist held intervals and remaining duration, and resume the same evaluated countdown on authoritative resolution rather than restarting the full default. Multiple applicable holds require all relevant release evidence. This gives neither service access nor a billing waiver.

Timer firing, resolution and guarded cancel share the local episode/aggregate fence. If resolution wins, a stale timer cannot cancel; if cancellation has already applied, later payment is recovered financially and does not resurrect service. New independent debt needs its own evaluated policy/episode rather than recycling an old timer. Suspend retains resources and the same occupancy claim; unchanged-key resume converts it once after its normal guards. A reduced capacity limit does not evict it. Dwell/TTL expiry alone never releases a place.

**RO04 cutoff, actual stop and local cancellation**

Customer confirmation requires authorization, accepted commercial eligibility and durable versioned cutoff evidence. Stage dependencies first; commit confirmation/intent generation, receipt and publication/recovery work atomically under the local cancellation fence. An uncertain reply is recovered by business identity. Neither an HTTP acceptance nor a failed/unknown durable write may promise confirmation. A future confirmed cutoff permits normal accepted service before it. Confirmation does not wait for resource completion.

Keep four distinct milestones: confirmed charging cutoff, actual qualifying resource stop, applied subscription cancellation, and completed data disposition. For ordinary accrual, use the earlier established bound among the current valid confirmed cutoff and authoritative qualifying cancellation/offboarding stop; an unknown bound is not invented. Actual state/stop timestamps are never backdated to the cutoff. Only applied cancellation releases normal active/retained occupancy and revokes normal desired service; retention/export is separately governed. Unknown stop may keep operational closeout pending but cannot extend tariff beyond a known cutoff.

Withdrawal/rescheduling must be authorized and durably confirmed before the currently applicable cutoff, with expected confirmation/intent revision. Serialize it against the original firing; publish the successor financial evidence and preserve history. An old queued worker cannot restore a superseded cutoff or stop obligation. At/after cutoff, unfinished resource work is not permission to restore charging; new service requires a separately supported guarded route. A response lost during rescheduling recovers the winner, not a second independent cancellation.

Provider stop outcomes bind the exact action/resource/generation and causal basis. An old stop success may establish historical runtime and charging evidence, but cannot complete a newer unrelated operation or undo current posture. A stop whose eligibility/cause is unknown is not silently classified. The accepted internal-stop split remains: qualifying platform fault/maintenance beyond its accepted SLA uses authoritative stop evidence; operator/abuse/policy stops follow suspension bill/waive terms and do not themselves create customer-confirmed cancellation. SLA thresholds and extra remedies remain external policy; no new number is invented.

Publish full versioned commercial replacements and successor seals for an established/corrected financial bound. [RB](../DESIGN.md#financial-consumer-boundary) preserves original usage coordinates, values the supported target and lets Billing correct erroneous customer tail once. No fake physical stop, zero catalog price, debt erasure, assumed refund amount/timing or collection-pause posture is introduced. Prior valid debt and explicitly agreed retention obligations remain separate. Unactivated paid Orders continue under AP capture/reservation recovery, not invented service facts.

**RO05 hold versus irreversible deletion**

Applied cancellation opens an independent retained-resource disposition. Freeze its anchor, policy and original deadline; the existing default window opens with applied cancellation. Export is a narrow accepted read-only capability, not normal service or permission to change resource ownership. Data presence, hold, export or delayed deletion alone creates no fee. If explicit retention terms authorize one, it has its own obligation and evidence rather than extending the old service tariff.

The delete work order issued by Entitlements carries the exact resource/disposition identity and schedule generation Subscriptions supplied, plus the expected hold-policy version, due-time and required prerequisite evidence. Deletion eligibility and destructive execution must be ordered against hold placement/withdrawal at the provider's authoritative execution boundary. A cached negative hold read plus idempotent delete is insufficient. The selected adapter must either share that atomic authority or implement acknowledged generation/hold fencing; no cross-gear database transaction or specific license/resource provider endpoint is required.

Hold receipt immediately blocks release of any further destructive generation to Entitlements, which fences in-flight orders, but the system must distinguish requested/propagating from confirmed protection at the provider boundary. If hold protection wins there, an old delete command is durably refused/unapplied. If irreversible deletion already won, the provider reports its actual applied/in-progress/partial outcome; do not promise that the new hold restored deleted data. Retain the hold for remaining protected scope, reconcile each resource and expose the conflict.

Unknown ordering blocks any claim of protected/completed disposition and requires authoritative recovery. Automatic deletion stays disabled for a provider unable to prove these semantics.

When the final applicable hold is released, create a new evaluated schedule generation with authorized release time, future eligibility deadline and current required notice evidence. The expired old deadline/notice and an old worker are never reused. The supported hold-release policy must supply that future schedule; missing/unsupported policy stays blocked, with no invented immediate deletion or new numeric extension. Release is not a delete command. A newer hold invalidates this new eligibility through the same execution fence.

Delete execution/recovery uses the original business identity. Timeout is unknown, not deleted or absent. Provider inventory absence alone does not prove this specific deletion completed; require correlated operation evidence or an agreed authoritative reconciliation proof. Partial multi-resource completion records each resource and retains the remaining protected/disposition work. A replay cannot delete a subsequently created/replaced resource under a recycled logical name; bind the immutable resource generation. Normal access must not be restored by a stale materialization result during this process.

**RO06 archive and evidence**

Archive requires the evaluated cancel dwell and completion of the current data disposition, including successor schedules after holds. It is not triggered by original deadline alone, payment success, inventory absence or queue delivery. A remaining hold or unknown delete keeps the relevant disposition pending. Archive neither settles outstanding money nor purges historical facts. Financial/attribution/receipt history follows its independent retention/hold contract so late usage and invoice reproduction remain possible after operational resources are gone. Operational deletion cannot erase source evidence needed for RB corrections.

This adds no audit subsystem or automatic financial-history purge capability.

**RO07 delivery and conformance**

| Owner role | Required delivery |
| --- | --- |
| Terms owner / Contracts / configuration | Authorized immutable bound-term/default evidence, applicability/successor outcomes and supported evaluation semantics; no event-triggered automatic subscription rebinding. |
| Subscriptions | Term/episode and cancellation/confirmation fences, durable timers and recovery, full fact/seal updates, independent disposition/hold/schedule generations and archive guards. |
| Selected entitlement/resource/hold provider | Monotonic desired-state enforcement, actual effective-time outcomes, pause/resume preservation, stable operation recovery and demonstrated hold-versus-delete arbitration. Native LM/OSS architecture is not a prerequisite. |
| Rating / Billing / financial owner | FA exact-obligation resolution/ordered outcomes and RB full correction/recovery across cutoffs, retention and payment uncertainty. Notice delivery remains outside current scope. |

| Case | Intervention | Required outcome |
| --- | --- | --- |
| RO-C01 | Auto/manual renewal and two workers race | One term occurrence, one applied extension/fact set; retries recover it. |
| RO-C02 | Contract expires/amends or defaults change mid-term | Running evaluated terms/clocks unchanged; next renewal selects explicit successor/default route with required prerequisites. |
| RO-C03 | Bound Contracts provider is unavailable | No assumed expiry/no-binding/default fallback; keep dependent renewal pending. |
| RO-C04 | Late financial success follows grace and suspension | Same renewal boundary with truthful service intervals; no duplicate term, fictitious service or terminal revival. |
| RO-C05 | Debt resolution races dwell cancellation while resume is denied | Resolution winning voids the old timer, retains resources/place; an already applied cancellation is not undone. |
| RO-C06 | Dispute pauses dwell across restart; duplicate/stale outcomes arrive | Same frozen remaining duration, scoped ordered resolution; no restarted full dwell or automatic access. |
| RO-C07 | Cutoff confirmation write/reply fails; stop is delayed | No confirmation without durable evidence; replay recovers a committed cutoff and preserves it despite late/unknown stop. |
| RO-C08 | Pre-cutoff rescheduling races an old worker | One current confirmation generation; stale work cannot reinstate old cutoff. Post-cutoff withdrawal cannot restore charging. |
| RO-C09 | Actual qualifying stop precedes scheduled cutoff; callback arrives late | Accrual ends at the authoritative earlier stop, not callback arrival; facts correct without falsifying state time. |
| RO-C10 | Customer cutoff, applied cancel and resource retention occur at different times | Charging, occupancy and data lifecycle each follow their own established milestone. |
| RO-C11 | Hold reaches provider before stale delete; reverse order also tested | Hold winner prevents deletion; delete winner is truthfully reported, never false protected/restored data. Unknown remains recoverable. |
| RO-C12 | Hold release follows expired retention; old schedule worker wakes | Fresh future schedule generation, stale worker rejected; missing required notice/policy blocks deletion. |
| RO-C13 | Delete response is lost or only some resources are deleted | Recover original operation and per-resource generation; no false completion or deletion of replacement resources. |
| RO-C14 | Archive dwell passes with held/pending disposition | Archive remains blocked; financial history/holds remain independent of operational cleanup. |
| RO-C15 | Late pre-cutoff usage or erroneous posted technical-tail charge | Original time/terms preserved; RB correction/remedy once, no generic refund/waiver or post-cutoff tariff. |
| RO-C16 | Suspend/resume under a reduced occupancy limit; stale enable reply after cancel | Same retained claim converts once if other guards pass; no stale restoration of cancelled service. |

These refine V03/V13/V14/V18 and AP/FA/RB recovery cases. They are specified scenarios, not executed tests. The abstract contract and reciprocal documents close this semantic package; exact provider/SDK mappings, independent ratification and executed outcomes remain capability/release gates. Required notice prerequisites are preserved without allocating a notification implementation.
