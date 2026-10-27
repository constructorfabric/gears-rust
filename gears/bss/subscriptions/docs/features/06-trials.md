---
description: "Free-period and credit trial lifecycle, initial eligibility, expiry and conversion to the accepted paid target."
---

# Feature: Explicit Trial Terms, Conversion and Expiry

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Start an accepted trial](#start-an-accepted-trial)
  - [Scheduled or early conversion](#scheduled-or-early-conversion)
  - [Extend or expire](#extend-or-expire)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Single-episode arbitration](#single-episode-arbitration)
  - [Boundaries and consumer evidence](#boundaries-and-consumer-evidence)
  - [Credit preparation and recovery](#credit-preparation-and-recovery)
- [4. States (CDSL)](#4-states-cdsl)
  - [Trial state](#trial-state)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Governed trial lifecycle](#governed-trial-lifecycle)
- [6. Acceptance Criteria](#6-acceptance-criteria)
  - [Trial integration cases](#trial-integration-cases)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-featstatus-trials-implemented`
- [ ] `p1` - `cpt-cf-bss-subscriptions-feature-trials`

**Status:** target design for optional PlanRevision free-period or monetary-credit trials and shared credit accounting. S-U12 schema, S-U15 provider/Ledger integration, bounded financial coverage/enforcement and executed recovery evidence remain release gates.

**Credit-trial activation contract.** Billing commits one reserved promotional grant to the accepted acquisition and exact activation operation before Entitlements prepares access. Its amount, currency, scope and duration are fixed. The commitment guarantees coverage from successful Subscriptions activation T. Entitlements establishes usable readiness under that commitment, with authorized pre-T preparation borne by the platform and no pre-T bonus drawdown. Subscriptions records T, episode, commitment and publication intent atomically.

Billing then records the same effective grant without another issuance decision. Delayed recording does not gate initial access, shift T/expiry or create another grant. Only a proved non-activation fence permits release. Unknown outcomes retain the commitment. Current cancellation, expiry and exhaustion still govern eligibility. Exact SDK/provider mappings, bounded exposure/enforcement and executed recovery evidence remain delivery gates.

## 1. Feature Context

### 1.1 Overview

Apply the accepted optional free-period OR monetary-credit trial block of Pricing PlanRevision. Use ordinary subscription states and composition boundaries. Preserve trial evidence and require the agreed settlement arrangement before paid-target conversion.

### 1.2 Purpose

**Requirements:** `cpt-cf-bss-subscriptions-fr-trials-not-a-status`, `cpt-cf-bss-subscriptions-fr-trial-commercial-pattern`, `cpt-cf-bss-subscriptions-fr-trial-provisioning`, `cpt-cf-bss-subscriptions-fr-trial-conversion`, `cpt-cf-bss-subscriptions-fr-trial-early-conversion`, `cpt-cf-bss-subscriptions-fr-trial-expiry`.

**Principle:** `cpt-cf-bss-subscriptions-principle-manifest-closed-enum`.

### 1.3 Actors

| Actor | Role |
| --- | --- |
| `cpt-cf-bss-subscriptions-actor-customer` | Accepts the trial/paid target, requests early conversion or cancellation. |
| `cpt-cf-bss-subscriptions-actor-product-manager` | Approves commercial trial policy authored in Pricing; Products supplies SKU identity/structure, while Contracts contributes only explicitly bound terms. |
| `cpt-cf-bss-subscriptions-actor-platform-operator` | Inspects and recovers existing trial operations; cannot extend or repeat a trial. |

### 1.4 References

- **PRD:** [PRD.md](../PRD.md); **Design:** [capability gates](../DESIGN.md#41-capability-gates-and-source-precedence).
- **Build prerequisites:** [foundation](01-foundation.md), [composition](02-composition.md), [renewal/retention](04-renewal-retention.md) and [entitlement posture](05-entitlements.md), as allocated in [DECOMPOSITION.md](../DECOMPOSITION.md).
- **Integration acceptance:** trial boundary/fact conformance with [commercial facts](08-commercial-facts.md) must pass before its billing effects are enabled; it is not an additional trial-feature build dependency. External trial/payment/Entitlements contracts remain the launch gates below.
- **Current trial policy:** extension and repeat trials are deferred, including with explicit Pricing permission. Preserve payer/plan history and refuse repeat acquisitions across subscription IDs; retry recovers the original episode. No additional bonus, stacking, rollover or reactivation. Early valid conversion preserves eligible unused credit under its original expiry and restrictions.
- **Required concrete parameters and owner contracts:** explicit trial identity, eligible products/duration, trial/paid terms, authoritative settlement evidence, retry/later paid association, detailed postpay due/dunning, compatible acceptance/hold handling and Entitlements target/expiry protocol under S-Q06. Settlement-scheme and prepay-failure default directions are accepted; accepted trial/paid-target start restrictions and original grant expiry remain binding. Applicable later paid routing additionally requires S-U01 association/recovery agreement.

## 2. Actor Flows (CDSL)

### Start an accepted trial

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-flow-trial-start`

1. Reject trial enablement until the applicable S-Q06 authoring/provider/runtime contracts, S-U12 terms/schema, S-U04 readiness, S-U07 financial guards and executed recovery evidence are satisfied. Credit trials additionally require S-U15 provider/Ledger integration and bounded financial coverage/enforcement. Free-period trials require no promotional-grant provider. Settlement-scheme direction approval does not satisfy these gates; missing trial policy or paid-target evidence cannot be inferred.
2. After owner contract acceptance, consume exact Pricing PlanRevision/customer receipt and select its optional free-period OR credit block. Free periods require explicit zero bindings; credit requires ordinary prices plus bonus amount/currency/applicability/duration in immutable policy, with customer grant/coverage supplied through S-U15. The existing plan/revision identity identifies the storefront offer. There is no separate TrialOffer catalog, variant combination, new status or Products trial-policy attribute.
3. Require Orders-origin acquisition and authoritative payer/plan eligibility/history; refuse repeat trials even under a new subscription ID. Retry recovers the same episode and grant. For credit, Billing commits one grant to the accepted acquisition and exact activation operation before preparation. Save its reference/digest in the preparation posture; Entitlements prepares usable access under that guarantee and reports held readiness. Authorized pre-T use is platform-borne and consumes no bonus.

   Subscriptions then atomically records actual T, episode/original expiry, commitment and recording outbox, making the same grant effective under Billing's guarantee. Billing records that result without new approval or an initial-access gate. Use the [S-U15 startup contract](06-trials.md#credit-preparation-and-recovery); unknown outcomes retain the commitment until exact resolution, and only fenced non-activation permits release. Later cancellation/expiry remains authoritative; recording cannot shift T, extend expiry or revive access. Financial coverage for the accepted paid target and current lifecycle/security guards remain separate.
4. Record attributable trial service/usage under the accepted policy. Free-period Rating uses explicit zero bindings; credit-trial facts retain ordinary tariffs and are valued by served time. Facts and corrections for service before the applied paid boundary retain trial classification even when received after conversion. Billing applies the same grant and accepted liability rules to that served interval. Subscriptions facts carry agreed policy/grant evidence references. They contain no bonus amounts, raw usage or balance math. Legitimate trial facts remain valid even if the paid target has not started. Missing money/balance never means free access or spend permission.

### Scheduled or early conversion

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-flow-trial-conversion-trl`

1. Authorize and replay the operation, then acquire the trial-episode and aggregate fences. An already converted/expired episode cannot convert again under another transport key.
2. Check pending cancellation/opt-out at or before conversion first. A pending or failed unresolved cancellation continues suppressing paid conversion.
3. If suspended, defer the scheduled conversion without advancing terms or paid posture. Resume re-evaluates it; terminal cancellation voids it.
4. Validate the already accepted paid target under the versioned Pricing trial contract and obtain current operation authority plus the applicable FA financial permission. Unchanged terms need no new customer assent merely for conversion/retry; changed or previously unaccepted terms require explicit assent through a supported route. An expired initial V1 hold is not paid-target authority. Prepay requires payment prescribed by agreed terms before the paid transition; a stored method alone is insufficient.

   Postpay requires accepted postpay terms and authoritative authorization to serve on credit, without conversion-time payment or a linked card. No universal capture mechanism is inferred. Relevant unknown/outage is not absence or permission; unresolved required evidence cannot authorize conversion or extend the trial, while invoice/postpay does not acquire an unrelated card-provider dependency.
5. If no applicable authorized settlement route exists at expiry, use normal trial expiry/revocation, with no automatic successor paid-target period/posture/facts, debt or setup charge caused by expiry; legitimate trial facts remain available for rating and bonus allocation. An authorized postpay route instead permits conversion and ordinary paid accrual; invoice/payment follow its agreed due date. Delinquency applies only once due and unpaid, then follows agreed recorded failure/retry/restriction policy.

   Definitive prescribed-prepay failure at trial end defaults to no paid start, recorded failure and retry through the agreed path, while normal expiry/revocation retains data. Trial extension is deferred; no automatic grace/free extension or paid-renewal grace is inherited. Stage the commercial Entitlements posture under the agreed asynchronous protocol; no synchronous materialization prerequisite is added.
6. Once the prescribed payment or authorized postpay prerequisites and normal guards succeed, commit one eligible transition to accepted paid terms with actual boundary, composition revision, commercial posture and frozen fact versions. Payment success alone proves neither application nor instant Entitlements access. Later payment starts accrual only at the valid applied paid transition under S-Q01; the waiting gap creates no retroactive coverage/debt. An expired/cancelled episode cannot use this conversion path again. Later paid acquisition requires supported guarded routing as below.

   For early valid guarded conversion, retain unused credit on the same grant for eligible paid expenses until original expiry under unchanged currency/product/item/charge-kind restrictions. Closing the trial by this conversion does not revoke the grant; no new grant, top-up or duration is created. The bonus does not satisfy prescribed upfront payment unless the agreed coverage/settlement contract permits it.
7. Preserve enclosing commercial period identity unless the accepted model explicitly creates a new period. Publish corrected served slices to Rating. S-Q03/S-Q04 select Rating valuation and identity per agreed chargeable action; trial conversion alone is not a fee trigger. Conversion-related one-time capability remains disabled until its expressly agreed charge terms, occurrence schema and S-U08/S-U13 contracts/fixtures exist; these approvals do not expand the trial profile.

Unknown payment remains recoverable under the financial owner contract, not definitive failure or success. Reconcile the original outcome before a retry can collect again; payment uncertainty cannot release protected prepaid rights, authorize conversion or extend trial access automatically. Existing paid-order remedy and pre-capture protections remain in force.

### Extend or expire

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-flow-trial-extension-expiry`

1. Refuse extension and repeat-trial requests before effects even with Pricing policy or operator approval. Retain history and original expiry; create no successor trial interval or new bonus.
2. For an active unconverted trial, duration expiry or authoritative credit exhaustion without eligible paid conversion follows the accepted cancel/end action, revokes trial access and retains data under existing policy. Revocation intent and completion are separately observable; no premature occupancy release follows. Expiry alone creates no paid period, debt or setup occurrence. Legitimate prior obligations and prepaid-order remedies survive.
3. Later new paid acquisition uses Orders and its normal guards; it cannot revive a terminal subscription or automatically reuse resources. Early conversion of the existing eligible trial remains in scope through its own flow.

Consume Billing's authoritative versioned grant expiry/exhaustion outcomes under the proposed S-U15 contract. For an active unconverted credit trial, serialize the resulting trial lifecycle intent against the episode. After early conversion has already applied, retained-grant expiry/exhaustion ends only bonus coverage; it does not itself stop or revoke paid service, duplicate conversion or reverse it. Remaining paid expenses follow the accepted paid-plan/settlement rules and applicable guards; no unsupported liability or waived payment prerequisite is inferred.

One shared grant covers eligible items once; a grant ID is not a setup occurrence. Balance observations are not access enforcement or paid permission. Stale/unknown outcomes cannot create debt, authorize spending, refill a grant or revive closed access. Billing applies canonical full Rating replacements to the same grant exactly once with original terms/result/invoice lineage; a still-valid/applicable grant may regain availability from a downward correction for an active trial or eligible paid usage after early guarded conversion, until the same original expiry.

After grant expiry, Billing corrects history/accounting while spendable credit stays zero and original expiration stays unchanged; correction cannot extend the trial, reopen the episode or repeat conversion. A later correction cannot automatically reopen a trial already ended by exhaustion, even before original expiry: restored accounting balance grants no service/spend permission for that closed episode and does not reverse an applied paid conversion. V1 has no reactivation route; a new acquisition is a fresh paid purchase; repeat trials are deferred.

For eligible accepted-trial expenses served before the authoritative applied paid boundary, the platform bears full rated value beyond bounded bonus coverage, including positive corrections, errors and accounting/stopping-delay or in-flight overrun; it creates no customer trial-overage debt. Usage actually served after a valid guarded paid transition follows accepted paid-plan/settlement terms after eligible bonus coverage. Classify by served time and the applied boundary, never rating/event/correction arrival; split crossing usage using supported evidence and never backdate paid conversion.

Bonus remains bounded; Rating preserves gross value and Billing accounts separately for bonus coverage, platform-borne amount and customer obligation. S-U15 exact evidence/provider/Ledger and detection/enforcement/recovery contracts plus other liability cases still gate launch; no instantaneous cap or unlimited free continuation is promised. Early guarded conversion retains unused credit as above; stacking and payer transfer are refused, and there is no new-period rollover or regrant in v1. A correction does not force a still-active trial to end; valid/applicable same-grant restoration remains usable until original expiry.

## 3. Processes / Business Logic (CDSL)

### Single-episode arbitration

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-trial-arbitration`

Early conversion and scheduled conversion use different command keys but share a trial-episode state guard in the committing transaction. Retain exactly one conversion record, predecessor trial terms, successor paid terms and correlation. Duplicate scheduled work after a successful early conversion observes the terminal episode and cannot emit another fact/posture. Cancellation suppressions are read under the same commit boundary.

**Initial-trial admission fence.** The no-repeat rule must hold across subscriptions, not only within one episode. Before Billing preparation, resource work or any trial-acquisition capture, atomically reserve the authorized payer/plan eligibility key for the stable Orders acquisition origin. A concurrent different acquisition conflicts; an identical origin replays or conflicts on changed content. A live reservation, unknown prior application or retained served-trial history cannot be treated as absent eligibility. Use an indexed unique key and the same scoped local transaction discipline as occupancy; authorization still applies independently.

At actual activation, lock/revalidate that admission association and mark it consumed with the episode, commercial state, actual T, receipt and outbox in the same local transaction. Consumed history remains after expiry/cancellation/paid conversion, so a new subscription ID cannot obtain another trial. Pricing initial eligibility must also pass; this local fence does not create a trial offer or replace financial/occupancy protection.

Release an unapplied reservation only through authoritative non-application settlement fenced against late activation, with any protected payment/claim and prepared-grant obligations resolved by their existing protocols. No timeout or draft cleanup proves non-application. A supported successor attempt retains the same acquisition/admission identity; an already applied episode is recovered, never treated as an unserved retry. Failed acquisition without service does not create served-trial history. This is recovery of initial admission, not permission for a repeat trial or terminal revival.

Exact persistence indexes and SDK transport types are implementation details; the serialized key, consumed-history rule and failure behavior are fixed by this design.

### Boundaries and consumer evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-trial-evidence`

Every change retains explicit accepted trial and paid intervals. Consumers receive actual served slices and current fact versions, with their immutable accepted terms. Expiry and conversion update the relevant fact versions/sets without losing earlier evidence. Trial and paid restrictions use the accepted asynchronous Entitlements materialization protocol, retaining both desired-boundary evidence and observed completion. Expiry does not prove instant revocation; no gapless-access guarantee or premature occupancy release follows from the partial direction approval.

### Credit preparation and recovery

Use the [trial evidence contract](../DESIGN.md#336-trial-terms-and-provider-evidence) and [initial readiness protocol](05-entitlements.md#initial-activation-readiness-contract). The conditional commitment, its effectiveness at T and Billing's later recording are separate steps. Exact SDK encoding, retained-history budgets and owner conformance remain delivery work.

| Evidence | Required meaning |
| --- | --- |
| Identity and association | Durable grant/benefit identity, seller and Orders acquisition/order/version/line, accepted receipt, designated consumer, activation operation and preparation generation. Retain episode/subscription association when established. Transport keys and new attempts cannot issue another grant. |
| Frozen source | PlanRevision, accepted policy and receipt references/digests; amount, currency, payer/beneficiary, eligible products/items/charge kinds and duration semantics. Billing validates authoritative Pricing evidence. No current-catalog substitution, FX or broadened scope. |
| Conditional validity | Reserved grant becomes effective if the exact activation applies at T. Original exclusive expiry follows T and frozen elapsed/calendar policy. Before T no bonus is spendable; delayed recording cannot postpone coverage or move expiry. |
| Lifecycle evidence | Authoritative operation/revision, episode, trial/paid/end boundaries, reason and retained-grant eligibility after early conversion. Effective served time differs from observation/recording time. |
| Recoverable outcome | Commitment/grant revision and digest, command identity/payload digest, pending/refused/applied status, ordered outcome sequence and read barrier. Accounting/allocation and financial completion remain separate provider statuses. Historical remaining balance is not access or paid-service authority. |

1. Reserve the payer/plan admission key before preparation or trial-acquisition capture. Billing then prepares one durable commitment for the accepted origin and exact activation operation/generation. Freeze the accepted amount, scope and duration without inventing T. Same-content replay returns that commitment; changed content conflicts. A new attempt needs the supported fenced association protocol and retains the same grant.
2. Persist its authenticated reference/digest in the preparation posture/outbox. Entitlements validates and binds it to the operation, posture, complete manifest and selected profile. Prepare usable access that can continue through T without waiting for Billing's later write. Adapter requests carry assignment scope and desired revision, never subscription ID. Pre-T and never-activated preparation is platform-borne and consumes no bonus.
3. At the final local decision, validate accepted terms, exact commitment, current order/financial/security/occupancy guards, deadline and held readiness. Serialize against withdrawal; a provider cannot withdraw coverage between validation and application without an acknowledged consumer fence. Commit actual T, episode/original expiry, coverage reference, state, receipt, posture and recording outbox atomically. No remote call occurs inside that transaction.
4. Billing consumes or recovers the applied proof and records the same grant effective at T without new issuance/eligibility approval. Verify the exclusive expiry against the frozen policy and Subscriptions boundary; inconsistent evidence refuses. Lost/reordered deliveries recover the original identity. Recording after cancellation or expiry preserves historical effectiveness and current closed eligibility; it cannot revive access or add duration.
5. Proved terminal non-activation permits settlement of only that prepared grant and its trial-admission reservation, with protected claim/payment obligations handled by their own protocol. Unknown or missing activation outcomes retain the commitment and require recovery/escalation. A timeout alone cannot release it. Applied activation takes the historical-recording path, followed by current cancellation/end handling.

Preparation and unresolved exposure must remain bounded under the selected service profile. Allocation-dependent preparation requires supported same-source continuity and release evidence. Consumable-limit readiness uses provider evidence; readiness-check requests carry a purpose marker, with execution and quota treatment chosen by the provider contract. Failure to support these guarantees disables the affected credit profile before launch.

**Outcome recovery.** Authorized reads by business/grant/operation identity return versioned evidence and eligibility reasons, with allocation and financial-completion statuses. Recover an immutable snapshot/barrier plus ordered subsequent outcomes. Report gaps, retention loss and partial pages explicitly. Not-found, unavailable and unknown are distinct; none proves zero balance, failed issuance, spend permission or customer debt. Unknown or stale outcomes cannot trigger paid conversion. Reconcile every grant outcome against the current episode/paid state before a lifecycle intent.

**Served-time classification.** Credit trials retain ordinary tariffs and legitimate gross served facts. Free trials use explicit zero bindings for every covered contribution. From authoritative T, eligible usage uses the same grant even while its Billing record is pending. Classify late or crossing usage by original served extents and applied trial/paid/expiry boundaries; missing evidence leaves allocation pending, never guessed prorating or arrival-time attribution.

Billing consumes canonical full Rating replacements and preserves prior result/allocation lineage. A correction can restore valid/applicable same-grant coverage once, without extending original expiry. Late historical usage remains classifiable after expiry, but expired credit never becomes currently spendable. A correction cannot revive an exhausted closed episode or undo paid conversion. Early guarded conversion retains eligible unused credit until its original expiry; later grant exhaustion/expiry changes coverage only. Eligible trial residual before the applied paid boundary, including positive corrections and stopping delay, is platform-borne. Post-boundary residual follows accepted paid terms. Excluded charges and other liability cases need their own agreed basis.

**Refusal and pending outcomes.** Missing trial inputs or unsupported variant/scope/precision cannot become free pricing or a default bonus. A changed identity/payload or scope/currency conflicts before mutation. A stale/gapped source requires head or history recovery. An unresolved replacement preserves the prior applied allocation and visible pending reason. Financial work pending is not a completed posting and cannot authorize another collection or release. No local balance observation replaces financial admission or access enforcement.

## 4. States (CDSL)

### Trial state

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-state-trials`

A trial episode is accepted/running/conversion-pending/converted/expired/cancelled. Suspension parks conversion while the subscription status is suspended; it does not manufacture another trial episode. Converted, expired and cancelled episodes cannot be reopened; repeat trials for the same payer and plan refuse across subscription ids; eligibility for a first trial comes from the accepted Pricing terms, not from the occupancy check. A late successful payment cannot revive a terminal cancelled subscription either.

Later paid acquisition must use a supported guarded route, preserving existing data/resource-retention policy; exact Orders/Subscriptions association and retry/recovery remain S-U01/S-U07/S-U12 gates as applicable. It creates neither a new trial nor a setup occurrence automatically.

## 5. Definitions of Done

### Governed trial lifecycle

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-dod-trials`

Ratify the remaining S-Q06 decisions and provider/consumer contracts, including authoritative settlement evidence versus uncertainty, retry/later paid routing, detailed postpay due/dunning and asynchronous expiry/revocation. Implement episode arbitration, scheme-specific settlement prerequisites, normal expiry and retained evidence, then demonstrate fixtures before the capability is enabled. S-Q03 integration, S-Q04 schema and Entitlements contracts independently gate their related conversion effects.

**Implements:** `cpt-cf-bss-subscriptions-flow-trial-start`, `cpt-cf-bss-subscriptions-flow-trial-conversion-trl`, `cpt-cf-bss-subscriptions-flow-trial-extension-expiry`.

## 6. Acceptance Criteria

- [ ] Trial launch while required S-Q06 authoring/provider/runtime contracts are open refuses before creating service, a hold or a commercial occurrence despite partial direction approval.
- [ ] No applicable authorized settlement route at trial end follows normal expiry/revocation with data retained under existing policy and no automatic successor paid-target boundary/posture/facts, debt or setup charge from expiry alone. Existing served trial facts remain available for rating/bonus allocation. Prior legitimate obligations, paid-retention terms and prepaid-order remedies survive.
- [ ] Definitive prescribed-prepay failure at trial end records failure and permits retry through the agreed path, while default expiry/revocation retains data and starts no paid period. No automatic free extension or paid-renewal grace applies; trial extension is deferred, so the current episode ends under its original terms.
- [ ] Later successful prescribed payment starts accrual at the valid guarded applied paid transition, with no retroactive debt/coverage for the waiting gap. Expired/cancelled episodes and terminal cancelled subscriptions are not reopened; the applicable supported paid-acquisition/association route must exist.
- [ ] Unknown payment is recovered as uncertainty, not failure/success/absence. Retry cannot double collect or release protected prepaid rights; it grants neither conversion nor automatic free extension, and payment success does not bypass guards or imply instant Entitlements completion.
- [ ] Accepted invoice/postpay plus authoritative credit-service authorization converts without a linked card or immediate payment, emits ordinary paid-period facts and accrues valid obligations. Invoice/payment follow agreed due terms; no delinquency or dunning is inferred before due/unpaid evidence. No existing/prepaid customer is automatically migrated to credit.
- [ ] Relevant owner unknown/outage is neither established route absence nor authorization and cannot enable conversion or extend trial. An unrelated card-provider outage does not block an otherwise fully evidenced invoice/postpay arrangement.
- [ ] A full Rating replacement corrects eligible pre-paid-boundary trial gross 90 to 110 against grant 100: bonus 100/platform 10/customer 0, even when received after conversion. Preserve gross and original source/tariff/version lineage; duplicate replacement has no additional effect or grant and cannot reopen a closed episode. Actual post-boundary usage follows accepted paid terms; erroneous customer posting uses the existing idempotent Billing correction/remedy path.
- [ ] Eligible delay-caused trial usage value 5 with bonus 2 allocates 2 to bonus and 3 to platform, without customer debt/negative bonus/top-up. Facts received after conversion retain their actual served-time classification; post-boundary usage instead follows accepted paid terms and crossing usage requires supported attribution evidence. Erroneous billed trial tail is corrected idempotently.
- [ ] Delayed Entitlements revocation remains observable as pending/completed; expiry alone proves neither instantaneous access removal nor permission to release occupancy before the applicable applied lifecycle transition.
- [ ] Early and scheduled conversion racing under different keys produce one conversion outcome.
- [ ] A cancellation intent, including a transiently failed firing, prevents conversion until explicitly resolved.
- [ ] Suspension parks conversion; terminal cancellation voids it; resume revalidates current accepted terms rather than using expired hold evidence.
- [ ] Conversion/expiry replay after restart preserves the same episode identity and consumer fact history.
- [ ] A crash or lost response between credit-trial activation and Billing finalization recovers the same prepared/grant identity and original T/expiry. Commitment-backed prepared access continues across T despite delayed Billing recording; a newer cancellation/revocation remains authoritative and late recording cannot revive it.

- [ ] Trial extension and repeat requests, including a new subscription for the same payer/plan, refuse before effects; policy permission cannot override deferral. Same-acquisition retries reuse the original episode/grant.

- [ ] Two Orders for the same payer/plan race before preparation/capture: at most one reserves trial admission. Lost replies replay the winner; a new subscription ID cannot bypass consumed history. Fenced unapplied settlement releases only its own reservation; a delayed activation cannot consume a released association.

### Trial integration cases

These TP cases define joint integration acceptance, not executed tests. Free-period trials require their Pricing/Orders/episode gates; credit trials additionally require the Billing coverage, allocation and enforcement profile.

| Case | Scenario | Required evidence before enablement |
| --- | --- | --- |
| TP-C01 | Storefront and immutable revision | Existing plan/revision identifies the trial block; no separate offer catalog. A 14-to-7-day revision change leaves previous customer terms intact; numbers are illustrative |
| TP-C02 | Free-period versus credit trial | Free variant has explicit zero bindings; credit variant retains normal tariffs, explicit amount/currency/applicability and a separate grant. Missing terms and combined variants refuse |
| TP-C03 | Multiple eligible items and issuance replay | One shared grant is issued once and consumed across items; retry never clones its full amount per item or duplicates issuance |
| TP-C04 | Rating correction/invoice retry | Full result corrects 30 to 25 at the same tariff for a valid/applicable grant of 100: remaining 70 becomes 75 once, spendable only until the same original expiry. Duplicate version restores no more; conflicting version/content refuses/recovers. No new grant, cash refund or repricing; draft/posted invoice correction and Ledger integration preserve lineage |
| TP-C05 | Exhaustion, delayed usage and unknown balance | Authoritative versions recover ordered outcomes; unknown is not zero/spend permission. Eligible delay-caused trial usage 5 with bonus 2 allocates 2 to bonus and 3 to platform, without negative bonus/top-up/customer debt. Enforce/recover behavior remains required |
| TP-C06 | Usage before, after and across paid conversion | Late trial facts remain trial liability even when received after conversion; actual post-boundary usage residual follows agreed paid terms. Split crossing usage using supported served-time/applied-boundary evidence, never arrival/correction time or backdated conversion. Preserve gross usage/value and idempotently correct any erroneous billed trial tail |
| TP-C07 | Positive full-result correction of eligible trial service | Grant 100, prior gross 90, corrected pre-paid-boundary gross 110: bonus 100/platform 10/customer 0 once; preserve gross, source/version and original tariff. Late arrival after conversion does not change classification. Actual post-boundary usage follows accepted paid terms. Replay cannot double-adjust, top up, refund cash or revive an exhausted trial; erroneous posted customer excess follows existing Billing correction/remedy |
| TP-C08 | Correction after grant expiry | For the same 100 grant and 30→25 correction, history records net consumption 25 and conceptual remaining 75, but spendable credit is 0. Original expiration is unchanged; no trial extension, new grant/top-up, cash refund, access revival or repeated conversion. Duplicate correction is a no-op; v1 has no closed-episode reactivation or same-subscription regrant, and a separate fresh paid purchase requires its supported Orders acquisition contract; repeat trials are deferred even with explicit policy |
| TP-C09 | Correction after exhaustion ended the trial | Before original expiry, full consumption 100 corrected to 95 restores 5 in accounting once, but does not restart the trial, grant service/spend permission for the closed episode or reverse an applied paid conversion. No new grant/top-up, setup trigger or cash refund; an active valid/applicable trial or eligible paid usage after early guarded conversion still permits same-grant restoration until original expiry |
| TP-C10 | Currency, charge-kind or scope mismatch | Unsupported application refuses; no FX/all-product default or automatic setup charge. Excluded/overage charges require an agreed paid basis |
| TP-C11 | Early guarded paid conversion with unused credit | Illustrative 100-credit/14-day grant, day-5 conversion with 60 left: same 60 covers eligible paid expenses until original day 14, under unchanged restrictions. No new grant/top-up/term or revived expiry; full-result corrections retain lineage and paid tariffs. Normal settlement guards still apply; no assumed upfront-payment satisfaction. Later retained-grant expiry/exhaustion ends only bonus coverage, with no paid-service stop/revocation or duplicate/reversed conversion; remaining charges follow accepted paid-plan/settlement rules and applicable guards |
| TP-C12 | Business origin and lost reply | Orders uses order/version/line; standalone acquisition is refused. Another transport key/attempt cannot create another preparation/grant for that business origin; conflict/unsupported rebind refuses and unknown outcome recovers the same identity |
| TP-C13 | Billing recording is delayed/lost after local activation | Commitment and usable readiness exist before T. Local activation makes the reserved grant effective at T; access continues without waiting for Billing recording. Recovery records the same grant once with original expiry, no new decision or double allocation. Unknown outcome is recovered, never assigned a guessed T. |
| TP-C14 | Coverage preparation races activation/cancellation; responses are lost | Proven terminal non-activation permits release, with pre-T/never-activated use platform-borne and no bonus drawdown. Unknown retains commitment and bounded preparation. Applied T winning requires historical grant recording even after later cancellation/expiry, without regrant or access revival. |
| TP-C15 | Concurrent allocation and source versions | Two eligible items racing for the same remainder cannot double-spend it; stale versions cannot overwrite or restore twice. Same-version/different-content conflicts; missing boundary/predecessor evidence stays pending |
| TP-C16 | Allocation committed, invoice/Ledger reply lost | Recover one stable financial-work identity; grant/allocation versions remain auditable, posting is not falsely complete, and retry creates no duplicate invoice correction/credit. Closed invoices follow the posted correction path |
| TP-C17 | Read/outcome gap or unknown balance | Snapshot/barrier and ordered outcomes expose gaps/retention loss explicitly; no empty-success fallback, zero balance, permission or inferred debt |
| TP-C18 | Requested trial exception | Extension/repeat/additional-bonus/transfer/stacking refuse regardless of Pricing permission or operator privilege. Repeat refusal applies across subscription IDs for the same payer/plan; replay of the same acquisition retains its original episode. Early-paid carryover remains unchanged |
| TP-C19 | Prepay failure and authorized postpay | Default failed prepay records failure/retry and expires (notification delivery is deferred); no waiting-gap debt on later valid start. Authorized postpay still needs no immediate payment/card |
| TP-C20 | Cancellation/replay and original paid rights | One local episode outcome; grant/financial recovery fences late effects. No duplicate collection or lost protected future-order commitment |
| TP-C21 | Retired catalog revision, expired offer after purchase or unsupported V1 | Accepted trial/paid-target terms remain recoverable and unchanged; current financial/lifecycle/security guards and explicit accepted restrictions apply. Unsupported initial acceptance capability refuses the profile before purchase. |
| TP-C22 | Free binding includes all covered contributions | Zero unit price with a nonzero covered mandatory fee is rejected as an incomplete free-period contract; ordinary paid bindings remain unchanged. |
| TP-C23 | Late or boundary-crossing usage while grant recording is delayed | Preserve T and original expiry. Pre-T use is platform-borne without bonus drawdown; eligible use from T is allocated once against the same grant under accepted scope and liability. Missing boundary evidence stays unresolved, not a fabricated balance, debt or extension. |
| TP-C24 | Paid-target assent versus operation authority | Unchanged accepted paid terms need no new assent on retry/conversion; current AP/FA authority is still required. An expired original V1 hold cannot authorize it. |
| TP-C25 | Late historical usage after grant expiry | Classify by original eligible served time and full source lineage; same-grant accounting correction never restores current spend permission or trial access. |
| TP-C26 | Variant-specific capability gate | A free-period profile requires no promotional-grant service; a credit profile cannot start without supported Billing coverage. Neither variant creates a card/capture prerequisite merely by being a trial. |
