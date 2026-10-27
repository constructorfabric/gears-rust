---
description: "Current Subscriptions decisions, ownership boundaries and integration gates."
---

<!-- CONFLUENCE_TITLE: [BSS]: Subscriptions: Decisions -->

# Subscriptions: Decisions

This register states the decisions for the [current PRD scope](PRD.md#11-purpose). Detailed procedures live in the feature documents; [provider requirements](UPSTREAM_REQS.md#25-provider-prerequisites-and-bilateral-acceptance) identify the required integration contracts and release evidence. A design decision does not prove provider implementation or another owner's contract acceptance.

<!-- toc -->

- [Current disposition](#current-disposition)
- [Lifecycle and commercial records](#lifecycle-and-commercial-records)
- [Ownership and recovery](#ownership-and-recovery)
- [Commercial valuation](#commercial-valuation)
- [Readiness and execution](#readiness-and-execution)
- [Integration delivery gates](#integration-delivery-gates)

<!-- /toc -->

## Current disposition

Subscriptions owns accepted customer terms, lifecycle decisions, commercial revisions, durable operations and money-free commercial facts. Entitlements owns applying commercial posture to quotas, licenses and resources, including adapter contracts and reconciliation. Pricing stores immutable commercial rules; Rating values accepted facts; Billing owns financial allocation, posting and correction.

Supported changes are immediate quantity increases and component additions. Orders future starts, cancellation schedules, normal renewal/trial timers and guarded early trial conversion remain supported under their capability gates. Separate customer acceptance confirmation, ramps, co-termination, collection pause, notification delivery, tenant/payer transfer, administrative historical correction, import, trial extension/repeat, standalone acquisition, scheduled composition/resume, linked subscription replacement and general base-plan changes are outside the current scope. Unsupported commands refuse before effects, including through operator wrappers.

Missing required notice or acceptance cannot be treated as satisfied. Legacy-system migration is not required.

Operational provenance, immutable history, authorization and recoverable domain events remain required. Dedicated audit functionality is outside the current scope. Current-system replay and Rating/Billing corrections remain required.

## Lifecycle and commercial records

**SUB-D-01: Scheduled lifecycle intents.** Cancellation supports immediate, end-of-term and explicit-date modes. Pending intents are durable. They can be withdrawn before application and are evaluated by the normal guards at their boundary. Scheduling is planning evidence, not applied service or a billable fact. Scheduled composition and resume are unsupported.

**SUB-D-02: Quantity changes.** Quantity increases and component additions are first-class transitions. One request has one expected revision, one effective instant and one atomic resulting composition. Existing lots retain their terms; the delta receives its own immutable accepted terms and purchase provenance. Decreases and removals are unsupported.

**SUB-D-03: Collection pause scope.** Collection pause is unsupported in the current profile.

**SUB-D-04: Ramp scope.** Contract-authored multi-step ramps are unsupported in the current profile.

**SUB-D-05: Activation dates.** Contract booking and the caller's requested start are distinct from actual service activation. Subscriptions records actual activation T locally after initial readiness and final guards. The subscription remains `draft` during preparation. A profile requiring separate customer acceptance cannot launch without that capability.

**SUB-D-06: Immutable ordering identity.** `orderingTenantId` is fixed at creation and supplies the subscription stream/partition identity. Creation-time commercial tenant axes remain explicit; tenant/payer transfer is unsupported.

**SUB-D-07: Commercial and monetary ownership.** Subscriptions publishes money-free recurring, usage-parent and qualifying one-time facts. Rating values them from frozen accepted terms. Billing applies the complete financial result, existing advances and final invoice rounding/corrections. Replay does not create another obligation or collection.

**SUB-D-08: One mutation path.** Every supported mutation, including create, renewal, intent withdrawal and archive, uses the durable operation envelope and local commit path. Creation has tenant-scoped business deduplication and starts the revision stream. Unsupported operations cannot enter through alternate administrative endpoints.

**SUB-D-09: Event authority.** Subscriptions owns its lifecycle, intent and commercial-fact events. State, operation outcome and publication intent commit atomically. Provider execution observations do not become a second source of commercial decisions.

**SUB-D-11: Draft cancellation.** A draft can be cancelled through the ordinary guarded operation. An unconfirmed draft has no automatic occupancy claim; its configurable housekeeping TTL has no financial effect. Protected purchase claims and unresolved preparation/payment effects survive draft cleanup and require their own fenced resolution.

**SUB-D-12: Renewal collection.** Collection-pause renewal bypass is unsupported; renewal retains its required financial guards.

**SUB-D-13: Term boundaries.** `autoRenew = false` reaches the normal end-of-term cancellation route. Renewal recovery advances an agreed term occurrence at most once and retains its accepted boundary and billing posture. A payment outcome alone does not resume service or revive a terminal subscription. [Renewal and offboarding](features/04-renewal-retention.md#7-renewal-and-offboarding-integration-contract) defines the applicable late-success and suspension cases.

**SUB-D-15: Manual change scope.** Immediate supported purchases use the ordinary atomic composition path. No ramp schedule or ramp-override operation is enabled.

**SUB-D-16: Nonpayment dwell.** Resolve and snapshot the applicable policy, provenance and deadline when the suspension episode starts. Configuration changes do not move that running deadline. The platform default is 90 days. Expiry submits guarded cancellation; only applied cancellation releases the subscription's occupancy claim.

**SUB-D-17: Required notice.** Renewal or continuation must satisfy the notice/assent prerequisites of its accepted terms. A price successor is not permission to amend those terms. Notification delivery is deferred, so a missing required notice blocks its dependent action.

**SUB-D-18: Cancellation money boundary.** Cancellation updates commercial obligations through immutable successor facts and seals. Subscriptions computes no refund, credit or early-termination amount. Billing handles financial correction and settlement under accepted terms and explicit cutoff evidence.

**SUB-D-19: Per-component facts.** A commercial obligation includes the component/interval identity within its subscription and period. Distinct component obligations cannot collide under one subscription-wide key. Rating may derive multiple calculation windows or lines from one fact.

**SUB-D-20: As-of attribution.** Facts describe the effective composition, quantities, payer, terms and served intervals of their economic time. Processing delay must not substitute current catalog data or current attribution. Complete historical evidence remains recoverable independently of event delivery.

**SUB-D-21: Component and fact identity.** Component intervals retain stable identities. Mid-period additions and terminal boundaries publish the complete affected commercial obligations, including earlier served intervals. Same-obligation quantity changes publish full successor versions; they do not create an independent monetary delta. Trial obligations retain their accepted terms and actual episode boundaries.

**SUB-D-22: Dwell lifecycle.** Authoritative payment resolution clears only the blocking dwell episode; a committed exit from suspension clears its applicable dwell. An authoritative dispute can hold the episode's clock under the selected policy. Retry recovers the same cancellation operation and recorded resource outcome. No pending cancellation, timer or timeout releases a retained place.

**SUB-D-23: Intent firing failures.** Transient dependency uncertainty retries with bounded backoff and retained suppressions. Definitive refusal becomes an explicit failed intent. If a state precondition is expected to clear, such as conversion while suspended, park the intent and pause its firing deadline until the unblocking transition. A terminal lifecycle outcome settles the pending work.

**SUB-D-24: One-time valuation.** A qualifying agreed action produces an amount-less fact valued by Rating from immutable accepted terms. Billing owns financial application. An advance is applied to the same obligation and cannot be collected again at activation.

**SUB-D-25: Reason-aware cancellation.** Cancellation carries its reason, effective cutoff, term and period evidence. Customer/operator early termination can be eligible for contractual treatment; natural term expiry and nonpayment exhaustion do not invent an early-termination fee. Exact financial consequences remain governed by accepted terms and Billing's contract.

**SUB-D-27: Billing calendar.** Subscriptions derives period identity from the frozen `billingAnchorPolicy` and anchor. A month-end clamp does not drift: January 31 reaches February 28/29 and then March 31. Rating uses the same geometry without independently creating a second recurring obligation.

**SUB-D-28: Chargeable-action occurrence.** Each separately agreed chargeable business action has one durable occurrence identity independent of prices and technical attempts. Retry, resume, trial conversion, cancellation/re-add, catalog change and reprovision do not create a fee by themselves. Correcting the original action retains its occurrence and publishes a successor fact/seal. Reserving an ID, prepaying or preparing resources does not prove the action occurred.

**SUB-D-29: PriceBook bindings.** Accepted bindings retain price IDs, revision/item identity, selected dimensions, product evidence and policy/invoice terms. Publishing a price revision does not change existing purchases. Renewal resolves under accepted renewal policy; an in-term binding's own `ends_on` requires explicit continuation authority. `effective_to` alone does not terminate a pin. Replay reads pinned immutable money.

## Ownership and recovery

**SUB-D-30: Commercial posture.** Subscriptions records `CommercialPostureV1` with the source revision and outbox. Entitlements applies it through provider-specific capabilities. A local subscription receipt does not prove applied provider access; initial activation consumes the readiness evidence defined by SUB-D-55–57.

**SUB-D-31: Pricing adapter.** The [Pricing integration](DESIGN.md#332-pricing-acceptance-and-accepted-time) uses the read, acceptance and sellability ports. An eligibility observation is not a durable purchase commitment. Accepted commercial evidence is separate from Rating calculation snapshots. Future-purchase and trial extensions follow the [accepted-acquisition](DESIGN.md#accepted-acquisition-evidence) and [trial-evidence](DESIGN.md#336-trial-terms-and-provider-evidence) contracts. The [foundation](features/01-foundation.md#acquisition-recovery) and [trial](features/06-trials.md#credit-preparation-and-recovery) features define recovery.

**SUB-D-32: Commercial fact authority.** Subscriptions owns immutable full fact versions, complete payer-specific commercial membership and exact seals. Usage parents open with the commercial period; Rating owns child-window work. Full-state attribution segments, facts, membership and source checkpoints are recoverable by pull. Commercial completeness, usage-source coverage and Rating calculation completeness are independent. Removal publishes explicit zero-obligation evidence; it does not erase history.

**SUB-D-33: Durable operation recovery.** Acceptance, local state and outbox commit together. Business create identity includes order/version/attempt/line and cannot be bypassed with another transport key. Settle unknown create under the same serialization boundary as create; negative settlement leaves a durable tombstone. Acceptance is distinct from terminal application. Unknown external effects retain recovery and cannot be discarded by expiry, withdrawal or compensation. Remote calls run outside the local commit lock; application revalidates local fences.

Technical retries retain the original attempt. A successor attempt requires proven predecessor non-application, safe effect settlement/quarantine, financial recovery and exclusive reassociation to the same valid acquisition. Preserve the existing claim, payment/commitment, charge occurrence and predecessor lineage. Applied predecessors are recovered. A changed order version is a commercial amendment, with explicit claim/commitment mapping and financial applicability; it is not a technical retry or automatic cross-payer funding transfer.

**SUB-D-34: Atomic composition.** Validate the entire resulting composition against one expected revision and effective instant, self-exempting the subscription's existing occupancy. Purchase retries create no second lot. Commit composition, lots, operation result, affected full facts, successor seal and outbox atomically. Unsupported multi-date changes refuse. [Changes](features/03-changes.md#apply-one-atomic-changecomposition) defines the procedure.

**SUB-D-35: Resource disposition.** Suspension preserves data-bearing resources. Cancellation separates confirmed charging cutoff, applied commercial cancellation, actual resource stop and deletion. Ordinary customer accrual ends at the confirmed cutoff despite delayed stop, or at an authoritative earlier offboarding stop. Actual runtime remains truthful. Retained data alone creates no fee; separately agreed retention terms may create an obligation.

Cancellation confirmation requires authorization, commercial eligibility and durable cutoff recording. A request receipt is insufficient. Withdrawal/rescheduling requires an explicit versioned confirmed action before cutoff and cannot restore past charges. After cutoff, service requires a valid guarded route. Platform-fault/maintenance stops beyond the accepted SLA end ordinary accrual at authoritative stop; operator/abuse/policy stops follow the accepted suspension billing posture and do not create a cutoff by themselves.

Explicitly bound agreed terms take precedence over versioned configuration. Freeze the selected values and deadlines. The [current platform defaults](PRD.md#platform-defaults-and-contract-binding) are 7-day grace, 90-day nonpayment dwell, 30-day retention and 90-day archive dwell with completed data disposition. Retention notices have 7-day and 1-day lead times; their scheduling and delivery are deferred. Hold prevents purge, and unavailable required notice blocks deletion. Final hold release creates a new recorded generation/deadline. A Contract ending mid-term leaves the running term unchanged; next renewal uses an explicitly bound successor or applicable platform defaults with required price-change notice.

Financial evidence retention is separate from resource-data retention.

**SUB-D-36: Trials.** Pricing PlanRevision carries an optional free-period block with explicit zero prices OR a monetary-credit block covering ordinary tariff value. Products owns SKU identity/structure; Billing owns grants, allocations and corrections; Rating retains gross valuation; Subscriptions owns episode boundaries and conversion. Paid conversion follows the accepted settlement scheme: confirmed prescribed prepayment or durable authorization to serve on credit. A card is not universally required. Unknown financial evidence grants no permission.

Definitive prepay failure causes no paid start or waiting-gap debt; normal expiry retains data, and later paid service needs a supported guarded route.

Credit trials require one operation-bound Billing grant commitment before preparation. Successful local T makes that same grant effective; later Billing recording is recovery, not a new access gate. Pre-T preparation consumes no bonus. Eligible accepted-trial value beyond the bonus before the applied paid boundary is platform-borne, including late positive corrections; actual post-boundary usage follows paid terms regardless of arrival time.

Full Rating corrections adjust the same grant once. Original expiry, restrictions and result lineage remain. After expiry, corrected accounting creates no spendable credit. Correction cannot reopen an exhausted/closed episode or reverse an applied paid conversion. Early guarded conversion preserves eligible unused credit on the same grant until original expiry. Concurrent initial trial acquisition is serialized by payer/plan; served-episode history prevents repeat trials across new subscription IDs. Extension, repeat, regrant, stacking, transfer and reactivation are unsupported.

Free-period delivery does not require a promotional-grant provider; credit delivery does.

**SUB-D-38: Documentation authority.** PRD owns observable requirements; DESIGN owns architecture; features own procedures and acceptance; DECOMPOSITION assigns each active FR/NFR one primary feature; UPSTREAM_REQS records dependency evidence; this register owns decisions. [UPSTREAM_REQS](UPSTREAM_REQS.md#25-provider-prerequisites-and-bilateral-acceptance) distinguishes required provider contracts from owner adoption and measured conformance.

**SUB-D-39: Suspended purchase refusal.** Quantity increases, seat purchases and component additions refuse while suspended at both submission and commit. Perform a separate authorized guarded resume before the normal active change. Purchase does not resume service or queue itself. If suspension wins, refuse the unapplied purchase and recover staged effects; if purchase applied first, retain its obligations and reconcile remaining delivery under the later suspension fence. Draft changes retain their separate S-U01 gate.

Renewal recovery and unrelated administration retain their own contracts.

## Commercial valuation

**SUB-D-40: Exact commercial seals.** Every immutable seal names exact `(fact_id, fact_version, payload_digest)` members. Billing accepts only complete results matching those entries. Changed commercial content/membership needs a successor seal. A newer Rating calculation for the same exact fact may produce a successor financial target under the existing seal. Commercial-head freshness, complete manifests, invoice-version fencing and unknown predecessor recovery still apply.

**SUB-D-41: Authorized continuation.** Automatic in-term continuation at `ends_on` requires explicitly accepted policy and satisfied notice/assent. Pricing selects a permissible successor; Subscriptions authorizes and publishes its full fact and exact seal atomically. Rating cannot substitute terms. Missing authority or covered pricing leaves full valuation pending; the known prefix is not a complete result. No implicit debt, free-service or shutdown policy follows.

**SUB-D-42: Explicit unresolved interval.** An established obligation with a known boundary but no authorized continuation publishes a full successor fact with an explicit `Unresolved` interval and successor exact seal. Keep known intervals and lot/quantity/purchase lineage. Rating advances the source head but leaves full valuation pending; Billing cannot substitute an older result or infer automatic reversal. Resolution creates another full fact/seal. Implicit gaps and unpriced new admission are invalid.

**SUB-D-43: One-time calculation basis.** `Once` means a flat action fee; `Units { quantity, unit }` freezes the positive exact action quantity and agreed unit. Rating refuses missing/incompatible basis instead of assuming one or current subscription quantity. Flat action fees do not multiply by seats; later increases do not change the original action. Distinct additional fees require separately agreed actions.

**SUB-D-44: Structured pending reasons.** Rating's SDK-readable status for an exact pending fact contains a nonempty blocker list: stable code, affected interval/lot/window/dependency, responsible source, evidence and expected resolution route. Subscriptions' unresolved reasons remain source authority; Rating owns its own coverage/recovery findings. Previous complete results identify their own source versions and cannot establish current completion.

**SUB-D-45: Empty, zero and pending.** A complete empty replacement removes only that fact's former monetary contributions through Billing's guarded replacement. An active zero-valued obligation keeps classified zero lines, accepted terms and provenance. Missing conditions/data yields structured pending without complete monetary delivery. Zero usage still needs coverage and applicable minimum valuation; a zero total cannot erase lines or prove refund completion.

**SUB-D-46: Recurring flat and pooled period floor.** A recurring flat applies once per component obligation and covered time under identical applicable terms, without seat/lot multiplication. Different simultaneous flat tariffs are unsupported; authorized sequential terms use separate slices. Recurring proration and supported pooled period floors use exact covered UTC seconds divided by declared billing-period UTC seconds. Sum eligible base amounts before one floor per price/subscription/period/currency/scale; use interval union, not overlapping sums.

Different price IDs retain separate floors. Cross-fact aggregation requires an owner contract: neither independent per-fact floors nor a new admission ban is selected. Billing rounds the financial result.

**SUB-D-47: Per-resource minimum.** Usage tariffs can require a separate minimum for each resource. Rating must preserve the resource identity; invoice lines and purchase lots cannot substitute for it. The pooled subscription profile also remains valid for its accepted terms.

**SUB-D-48: Usage kind and resource group.** Minimum policy distinguishes eligible usage from target identity: for example, storage for one container, traffic for another, or storage pooled across a node group. A node group is not a BillingGroup, component, lot or invoice line. Historical group membership and resource evidence must support valuation.

**SUB-D-49: Tariff-selected scope.** Individual and group minimum scopes are alternatives for the same eligible usage under one accepted tariff. Apply the selected scope once; do not stack individual and group floors or infer a hierarchical residual top-up. Different usage kinds can select different scopes. Preserve the selection in accepted terms independently of invoice grouping.

**SUB-D-50: Monetary usage minimum.** Rate eligible usage, then apply `max(0, applicable_minimum_amount - eligible_base_amount)` once per selected target/window. Group scope sums eligible member amounts before the floor; individual scope evaluates each resource. Minimums do not inflate metered quantity.

**SUB-D-51: Minimum window.** The tariff selects hourly or subscription billing-period monetary minimums. Preserve the window and amount in accepted terms. An hourly minimum does not determine invoice frequency or imply simultaneous hourly and period floors.

**SUB-D-52: Partial hour.** Any eligible partial hour receives the full hourly monetary minimum once per selected target/window, without time proration or lot/member multiplication. Fifteen eligible minutes with a 1 EUR hourly minimum retain a 1 EUR floor; a larger rated base remains payable at its calculated amount. Completeness evidence remains required.

**SUB-D-53: Calendar hours.** Hourly windows follow calendar-hour boundaries. Service from 10:50 to 11:10 spans two minima, each compared with that hour's eligible base. Excess in one hour cannot offset another. Half-open attribution does not charge the next hour solely because service ends on its boundary. Exact timezone and lifecycle/group-change mapping require the owner contract.

**SUB-D-54: Partial billing period.** Individual/group billing-period tariffs explicitly select full or proportional minimum; no implicit default is selected. Preserve the mode in accepted terms. A 30 EUR minimum over 30 days with 10 eligible days gives a 30 EUR floor in full mode or 10 EUR in proportional mode. Eligible-duration/group-membership evidence and exact SDK mapping remain integration work. Existing pooled-period terms and the full partial-hour rule retain their own semantics.

The [Rating contract](features/08-commercial-facts.md#33-derive-stable-facts-and-complete-accepted-term-slices) and [minimum-fee contract](features/08-commercial-facts.md#33-derive-stable-facts-and-complete-accepted-term-slices) define the required contracts and examples. Resource/group identity, historical membership, cross-price eligibility, nonlinear allocation, full-result correction and provider conformance must be resolved before enabling affected profiles.

## Readiness and execution

**SUB-D-55: Initial readiness before active.** Initial usable access precedes local active/T and service-period start. Subscriptions persists the activation operation and preparation posture before dispatch. Entitlements owns the complete immutable required-assignment/producer manifest and reports an aggregate result bound to that exact operation/preparation revision/digest. An explicit empty manifest is required for a no-preparation profile. Subscriptions validates its binding and all final guards, then commits active state, fresh local T, period start, applied receipt and outbox together.

Required initial assignments include account creation/linking, included deployment and usable permissions; first login and unrelated later resource creation are not prerequisites. Late/unknown results remain recoverable, and no readiness result is not proof of non-execution. License Resolver is read/check only; issuing/feed belongs to the selected execution owner. Renewal, guarded resume, conversion and active modifications retain their separate contracts. See [the consumer readiness protocol](features/05-entitlements.md#initial-activation-readiness-contract).

**SUB-D-56: Entitlements ownership.** Entitlements owns rule semantics, provider-desired state, readiness manifests, adapter contracts and reconciliation. Subscriptions publishes `CommercialPostureV1` atomically with its source revision and consumes `ReadinessObservationV1`. Pricing stores quota/grant rules on PlanRevision and freezes them in acceptance. Adapter requests carry authorized scope, assignment and desired revision, never `subscriptionId`.

Disposition/hold changes publish complete snapshots in newer posture revisions even without a lifecycle status change; receipt-only persistence does not create a new commercial revision. Allocation preparation uses the authorized operation/scope and purchased capacity, counts real occupied resources before T and retains quota/source/member/allocation identities through T. Platform-borne cost does not erase occupancy. Fenced non-activation stops new allocation; occupied capacity is freed only by confirmed resource release and quota settlement. Shared-resource cleanup requires explicit authority.

Checks can carry `purpose = readiness_check` so providers recognize preparation. The provider selects inspection, dry-run or real execution and its accounting; the marker grants no permission, exemption, free quota or success guarantee. No consuming probe or service allowance is mandatory. Real effects still require idempotency, provenance and recovery. Counter/window identity survives activation. The current quota scope remains allocation-only; this recognition hook does not enable deferred consumable-limit enforcement.

Purchased capacity remains the desired commercial cap. An independent provider restriction is an observed discrepancy, not a purchase rewrite. Known incompatible offers cannot be accepted/captured; later discovered incompatibility retains the obligation and remediation. Required initial readiness cannot pass merely because a smaller environment works. Observations have their own ordering; no implicit counter reset, eviction or refund follows.

**SUB-D-57: Held readiness and activation resolution.** Entitlements holds prepared assignments under a durable commitment for the exact activation operation/preparation revision/manifest. Subscriptions serializes activation and ordinary withdrawal on one local fence and publishes the immutable terminal resolution through the posture contract. No remote call runs under that local lock. Timeout, absent/unreachable reads or failed guards do not authorize cleanup.

Both `applied(T)` and `closed_without_activation` identify authenticated namespace/scope, operation, preparation revision/digest and decision version. Applied also binds the complete manifest and commitment. Closure works before a manifest/commitment exists or arrives; it carries known bindings, fences late preparation/readiness and lets Entitlements recover all partial or later-discovered effects of that generation. Old closure cannot affect a successor. Applied continues the same assignments; proven non-activation allows only authorized cleanup.

Independently authorized emergency security blocking remains immediate and reports correlated evidence. An already committed activation remains historical truth and follows subsequent guarded lifecycle/financial policy. Held readiness is not an uptime guarantee. Authorized pre-T and never-activated preparation use is platform-borne, creates no ordinary customer service charge and consumes no trial bonus. Profiles bound exposure and supply durable settlement without changing accepted purchase validity or financial admission.

Each attempt snapshots preparation-policy version, authorized start and deadline; waiting for the agreed future start is separate. Expiry/definitive failure closes that attempt at the activation fence; an earlier applied result retains T. Orders remediates the same purchase/payment/claim, without automatic purchase cancellation/refund. Unknown outcome retains effects. Successor admission requires predecessor non-application and safely settled/quarantined effects under generation fences. Retries, restarts and profile edits cannot extend the deadline or bought start constraints.

## Integration delivery gates

The behavior is selected; the remaining work is concrete integration design, independent owner acceptance and executed conformance. The [provider register](UPSTREAM_REQS.md#25-provider-prerequisites-and-bilateral-acceptance) names capability gates. An adapter that cannot satisfy the contract keeps that profile disabled.

| ID | Selected behavior | Required integration work |
| --- | --- | --- |
| S-Q01 Activation time | Freeze purchase terms and future coverage at acceptance; Subscriptions records actual T after readiness and current execution guards. Ordinary quote/catalog drift cannot reprice or invalidate the purchase | Versioned purchase evidence, Products ordering, explicit bought start constraints, terminal-decision recovery and capture/activation fences |
| S-Q02 Occupancy | Authoritative versioned key/limit; separate active, retained suspended and order-reserved counts share capacity. Confirmation reserves before capture; future bookings count from confirmation. Activation consumes its own claim; resume reuses its retained claim. Applied cancel releases only its own claim. Lowered limits preserve protected rights and refuse new excess admission | Exact count/claim evidence, owner/version mapping, cancellation/capture release arbitration and safe supported key movement. Unconfirmed-draft cleanup has no claim effect; confirmed unpaid prepay keeps the claim until its agreed payment deadline and fenced order cancellation. Paid/unknown rights survive TTL and draft rebuild |
| S-Q03 Financial ownership | Subscriptions supplies qualifying money-free facts; Rating values; Billing posts/corrects/applies advances; payment and tax owners retain their duties | Accepted-binding/calculation evidence, complete replacement routing, invoice-version fences and financial recovery under S-U08/S-U13 |
| S-Q04 One-time occurrence | One durable identity per separately agreed chargeable action; technical retry or lifecycle movement alone creates no fee | Exact schema, applied-action evidence and replay/correction conformance; preserve financial lineage without duplicate emission |
| S-Q05 Cancellation and retention | Confirmed cutoff ends ordinary charges independently of physical stop. Resource stop, applied cancellation, deletion and financial settlement remain distinct | Versioned confirmation/withdrawal, actual-stop evidence, hold/delete fences, Billing correction/remedy and provider mappings under S-U05/S-U13 |
| S-Q06 Trials | Optional PlanRevision free-period or monetary-credit terms; original expiry, same-grant corrections/carryover, no terminal revival; Billing commitment before preparation becomes effective at T | Pricing acceptance/paid-target schema, Billing grant and coverage enforcement, served-time evidence, Rating/Billing/Ledger mapping and exact bounds under S-U12/S-U15 |
| S-Q07 Operating budgets | PRD §7 is the product workload/latency/capacity/availability/recovery target | SRE/consumer load, outage and restore evidence under S-U11; specified budgets are not measured results |

Financial admission binds the exact acquisition/operation and accepted terms. Initial activation consumes the retained allocated payment or durable future postpay commitment, without another debit or credit assessment. Technical proof expiry and later risk/limit changes do not revoke that basis. Explicit applicable financial dispositions are versioned and fenced against local application; missing evidence is recovered under the same acquisition. Ledger accounting is evidence, not admission authority.

Orders Lifecycle owns durable capture-start arbitration against acquisition cancellation after place and terms are protected. Workflow transports and recovers; Payments deduplicates the stable capture identity. Cancellation winning prevents late debit. Capture winning preserves cancellation/remedy rights. Unknown capture keeps protection; release requires authoritative resolution and capture/activation fences with the applicable remedy, not an assumed refund amount or timing. Multi-line outcomes retain per-line obligations; successful lines are not automatically cancelled by another line's failure.

Explicit compensation has its own authorization and evidence.
