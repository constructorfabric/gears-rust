---
description: "Authorized historical recovery, resource attribution, usage-scope proofs and external consumer contracts."
---

# Feature: Consumer Recovery and Attribution Contracts

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Recover a complete authorized commercial range](#21-recover-a-complete-authorized-commercial-range)
  - [2.2 Publish or correct historical resource attribution](#22-publish-or-correct-historical-resource-attribution)
  - [2.3 Produce batched ownership proofs for Rating windows](#23-produce-batched-ownership-proofs-for-rating-windows)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Route control-plane operations through their owning feature](#31-route-control-plane-operations-through-their-owning-feature)
  - [3.2 Consume current PriceBook providers and serve frozen bindings](#32-consume-current-pricebook-providers-and-serve-frozen-bindings)
  - [3.3 Recover Billing inputs without inferring invoice state](#33-recover-billing-inputs-without-inferring-invoice-state)
  - [3.4 Consume versioned Contracts terms](#34-consume-versioned-contracts-terms)
  - [3.5 Separate policy, materialization and resource evidence](#35-separate-policy-materialization-and-resource-evidence)
  - [3.6 Consume payments and trigger notices](#36-consume-payments-and-trigger-notices)
  - [3.7 Consume product identity and occupancy policy](#37-consume-product-identity-and-occupancy-policy)
  - [3.8 Recover gaps, cursor loss and retained history](#38-recover-gaps-cursor-loss-and-retained-history)
  - [3.9 Replace attribution without erasing financial history](#39-replace-attribution-without-erasing-financial-history)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Scope proof readiness](#41-scope-proof-readiness)
  - [4.2 Recovery exceptions](#42-recovery-exceptions)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Reads are sufficient without events](#51-reads-are-sufficient-without-events)
  - [5.2 Authoritative attribution and bounded proof production](#52-authoritative-attribution-and-bounded-proof-production)
  - [5.3 Boundary adapters preserve their owner's decisions](#53-boundary-adapters-preserve-their-owners-decisions)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-featstatus-consumer-contracts-implemented`
- [ ] `p1` - `cpt-cf-bss-subscriptions-feature-consumer-contracts`
## 1. Feature Context

### 1.1 Overview

Serve authenticated, bounded recovery of commercial facts, bindings, seals and resource attribution. Produce historical ownership proofs that let Rating build a local projection and distinguish a proven empty scope from missing evidence.

All Subscriptions ports described here are **target consumer/producer contracts**, not delivered APIs. The Pricing providers exist; their Subscriptions adapters are not yet implemented. Inventory barriers, usage coverage and remote authorization remain explicit owner dependencies in [UPSTREAM_REQS.md](../UPSTREAM_REQS.md).

### 1.2 Purpose

**Requirements**: `cpt-cf-bss-subscriptions-fr-event-consumers`, `cpt-cf-bss-subscriptions-contract-rating-read-model`, `cpt-cf-bss-subscriptions-contract-billing-handoff`.

**Supporting requirements (primary owner elsewhere):** `cpt-cf-bss-subscriptions-fr-event-payload-completeness`, `cpt-cf-bss-subscriptions-fr-billing-traceability`, `cpt-cf-bss-subscriptions-fr-tenant-axes`, `cpt-cf-bss-subscriptions-fr-delegation-proofs`, `cpt-cf-bss-subscriptions-fr-accepted-price-bindings`.

**Additional requirements**: `cpt-cf-bss-subscriptions-fr-attribution-segments`, `cpt-cf-bss-subscriptions-fr-usage-scope-proofs`, `cpt-cf-bss-subscriptions-fr-billing-recovery`, `cpt-cf-bss-subscriptions-fr-operation-recovery`.

**Primary NFRs**: `cpt-cf-bss-subscriptions-nfr-horizontal-partitioning`, `cpt-cf-bss-subscriptions-nfr-availability-recovery`.

**Principles**: `cpt-cf-bss-subscriptions-principle-effective-dated-composition`, `cpt-cf-bss-subscriptions-principle-fail-closed-gate`.

### 1.3 Actors

| Actor | Role in Feature |
|---|---|
| `cpt-cf-bss-subscriptions-actor-rating` | Recovers facts, projects historical attribution, requests ownership scopes in batches |
| `cpt-cf-bss-subscriptions-actor-billing` | Discovers due groups and recovers their complete composition |
| `cpt-cf-bss-subscriptions-actor-pricing` | Serves purchase acceptance, immutable accepted terms and authorized historical recovery |
| `cpt-cf-bss-subscriptions-actor-oss-provisioning` | Supplies authoritative resource readiness/deletion and historical inventory through its agreed owner contract |
| `cpt-cf-bss-subscriptions-actor-platform-operator` | Resolves source-loss, ambiguity and orphan exceptions with an audit trail |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md), consumer interfaces, ownership and retention requirements.
- **Design**: [DESIGN.md](../DESIGN.md), external contract field groups, authorization, recovery consistency and persistence.
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md).
- **Build prerequisites**: [Foundation](01-foundation.md), [composition](02-composition.md) and [commercial facts](08-commercial-facts.md), matching DECOMPOSITION.
- **Integration acceptance**: exercise provider adapters with renewal/retention, entitlement posture and retained tenant/attribution history after their contracts exist; they are not reverse build prerequisites for the recovery read contract. Production scopes, adapters and recovery retain their S-U02/S-U03/S-U04/S-U06/S-U07/S-U11 and S-Q07 gates as applicable. A test double does not close an owner dependency.
- **Owner contracts:** [Pricing design](../../../pricing/docs/DESIGN.md), [Rating contract](../DESIGN.md#rating-result-contract), [financial consumer boundary](../DESIGN.md#financial-consumer-boundary) and [activation design](../DESIGN.md#332-pricing-acceptance-and-accepted-time).

## 2. Actor Flows (CDSL)

**Use cases**: service-to-service recovery and ownership proof; no standalone user interface.

### 2.1 Recover a complete authorized commercial range

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-flow-boundary-con`

**Actor**: `cpt-cf-bss-subscriptions-actor-rating` or `cpt-cf-bss-subscriptions-actor-billing`.

**Success Scenarios**: Rating recovers an open usage period before invoice due time; Billing discovers an entirely missed group; a new consumer rebuilds from reads after every event is lost.

**Error Scenarios**: unauthorized tenant/group, cursor scope mismatch, expired recovery snapshot, retained-history loss, missing group or unsealed composition. An unavailable or incomplete answer never counts as an empty successful result.

**Steps**:
1. [ ] - `p1` - Authenticate the consumer service identity and authorize its explicit tenant axes, requested business keys and range. A caller-supplied seller/payer/resource tenant does not establish permission. - `inst-subscriptions-read-authorize-range`
2. [ ] - `p1` - Use `period_facts` for periods overlapping the requested interval or `groups_due` for Billing's due groups; then use `facts` for a named group/composition version. `bindings` returns stored historical bindings, not a fresh price resolution. - `inst-subscriptions-read-select-history`
3. [ ] - `p1` - Pin the read's visibility boundary and return bounded pages with the same scope, query and snapshot identity. Include whether the requested range is complete and the continuation necessary to retrieve the remainder. - `inst-subscriptions-read-pin-snapshot`
4. [ ] - `p1` - The consumer ingests every version using the same validation and business identities as event delivery. A replay of a partial page cannot create duplicate financial input. - `inst-subscriptions-read-ingest-versions`
5. [ ] - `p1` - Only a fully read, consistent snapshot proves that the enumerated range is complete. If an intervening authorization change or retention limit invalidates continuation, explicitly fail and restart a fresh authorized read; never silently skip the gap. - `inst-subscriptions-read-prove-completeness`

### 2.2 Publish or correct historical resource attribution

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-flow-publish-attribution`

**Actor**: `cpt-cf-bss-subscriptions-actor-oss-provisioning`, through authenticated resource lifecycle input.

**Success Scenarios**: readiness opens a segment from the effective ready instant; deletion closes it; a correction emits a new full-state version while preserving all prior versions and affected financial lineage.

**Error Scenarios**: source sequence gap, no eligible historical line, ambiguous historical match, unsupported transfer boundary, contradictory resource version. Persist the reason and withhold an ownership seal for the unresolved scope.

**Steps**:
1. [ ] - `p1` - Deduplicate the provider resource event and retain its resource version, effective instant and lifecycle sequence. Resource tenant is authoritative; usage arrives without an invented subscription identifier. - `inst-subscriptions-attribution-deduplicate-event`
2. [ ] - `p1` - Resolve eligible commercial lines as of readiness, using historical ownership, SKU/usage type and sale applicability. Do not use the line active when the message happens to arrive. - `inst-subscriptions-attribution-resolve-history`
3. [ ] - `p1` - On one valid match, store the full replacement segment with subscription/line/item, seller/payer, usage-type version and half-open effective interval. On no match or multiple matches, store an explicit exception with evidence; never choose the first line. - `inst-subscriptions-attribution-record-match`
4. [ ] - `p1` - Assign a monotonically increasing sequence for the resource-tenant attribution stream in the same transaction as the segment version and outbox event. Full replacement lets the consumer reconstruct a close even if it missed the opening event. - `inst-subscriptions-attribution-commit-sequence`
5. [ ] - `p1` - Retain readiness/deletion ordering gaps for recovery. A current closed line can still own earlier valid usage. Any deprovision action uses the platform policy path; it is not a command hidden inside an attribution event. - `inst-subscriptions-attribution-retain-gaps`

### 2.3 Produce batched ownership proofs for Rating windows

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-flow-scope-batch`

**Actor**: `cpt-cf-bss-subscriptions-actor-rating`.

**Success Scenarios**: a batched request for windows of one fact version returns immutable sealed ownership scopes or explicit per-window pending outcomes. An empty scope is proven from historical inventory.

**Error Scenarios**: unknown/mismatched fact version, unauthorized inventory scope, stale or incomplete inventory snapshot, unapplied lifecycle barrier, unresolved orphan/ambiguity, partial range, lost source history. A timeout never proves zero resources.

**Steps**:
1. [ ] - `p1` - Authorize fact/group, resource tenants, the exact requested windows and the inventory snapshot. Bound batch size and range using the published contract; numeric limits require the S-Q07 workload agreement. - `inst-subscriptions-scope-authorize-windows`
2. [ ] - `p1` - Obtain an immutable historical inventory snapshot and its lifecycle barrier from the inventory owner outside a local write lock. It must include deleted resources and chargeability intervals overlapping each requested served extent. - `inst-subscriptions-scope-read-inventory`
3. [ ] - `p1` - Catch up source lifecycle history through the named barrier and reconcile it against historical attribution. Resolve discrepancies or report each affected window pending with the required barrier/reason. A missing source capability is a dependency failure, not an empty inventory. - `inst-subscriptions-scope-reconcile-barrier`
4. [ ] - `p1` - Recheck local fact, attribution and inventory evidence identities; store each proof immutably with its fact ID/version, BillingGroup/window, expected resource/usage-type intervals, inventory snapshot/barrier, attribution sequence and digest. - `inst-subscriptions-scope-store-proof`
5. [ ] - `p1` - Return explicit results for every requested window, including pending results. Paginated proof batches retain the same request/snapshot and disclose unfinished ranges. `UsageScopeSealed` may announce completed proofs; consumers can recover them by identity without the event. - `inst-subscriptions-scope-return-results`
6. [ ] - `p1` - Rating independently requires its local projection to have applied the proof's attribution sequence. Scope proves who could have generated usage; the usage emitter separately proves coverage and the collector supplies recorded usage. Subscriptions does not certify either of those facts. - `inst-subscriptions-scope-consumer-completeness`

## 3. Processes / Business Logic (CDSL)

### 3.1 Route control-plane operations through their owning feature

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-control-plane-con`

Adapters preserve authenticated command scope, durable operation identity, expected aggregate revision and payload digest. An exact replay returns the existing result. A conflicting payload is refused. `Pending` is an acceptance receipt only. Order-originated create additionally uses the business identity `(order_id, order_version, attempt_id, line_id)` under SUB-D-33. A different transport key for that same identity returns the retained outcome or a conflict. It cannot create a second subscription.

`settle_create` serializes with creation under that exact identity, and its no-create tombstone prevents a delayed request for that attempt from committing. Consumers cannot rule out a delayed create from `get_operation` alone.

An execution retry preserves `attempt_id` and recovers the original operation. A new `attempt_id` is not a replay. Under the [successor-attempt protocol](01-foundation.md#acquisition-recovery), require authoritative predecessor non-application, create/activation suppression, resolved financial outcome and authorized lineage for the same still-valid acquisition. The local exclusive association change preserves the same claim and financial references; partial provider rebind keeps activation pending. Recover an already applied predecessor without recreating it; cancelled acquisitions and amended versions are not silently admitted through this recovery profile.

Retain cancellation/refund remedy constraints and explicit accepted purchase start conditions. Exact Orders/Workflow/provider contracts remain S-U01/S-U07/S-U10 gates; unsupported succession refuses, and unknown outcomes must be recovered first.

A changed commercial order version follows the accepted [amendment direction](01-foundation.md#acquisition-recovery), not same-acquisition attempt recovery. Require Orders' permitted state/fields, fresh commercial assessment and required approvals/acceptance, then explicit line/claim/commitment association and Billing/Payments applicability to the new version. Prior funding cannot automatically cross a payer change; indicative totals cannot drive collection. Fence superseded commands at their effect owners, retain unresolved old protection/financial obligations and expose pending handover. An order-version event or a local reference update alone proves neither transferable capacity nor payment.

Exact provider mappings remain gated; this adapter adds no amendment path during fulfillment.

Composition operations, withdrawal, draft void and compensation delegate to lifecycle features. The purchase-state rule requires adapters and direct/operator wrappers to refuse new seat purchases/increases/additions on suspended targets at submission and commit; a non-terminal state alone is not enough (SUB-D-39). Return a specific invalid-state refusal requiring separate guarded resume before normal active change, not a pending/queued purchase. Keep the S-U01 draft gate separate.

Recover an already committed purchase by its original operation identity even after suspension; its outbox must reconcile under the newer restricted revision without erasing historical obligations or restoring access. Renewal recovery and unrelated administration retain their existing routing and guards. This adapter follows IA-2026-10-18 initial readiness: an accepted/approved operation remains pending while the subscription is draft, and applied success is returned only after all initial usable service requirements and final guards complete at local T.

Orders Workflow consumes the applied result, not a broker receipt or a provider ready event. This adapter carries the acquisition's retained payment allocation or accepted future postpay commitment into activation, together with explicit disposition fences. Complete saved evidence requires no live financial approval; missing evidence is recovered by the same identity without new credit assessment or collection. Technical expiry and changed credit limits do not revoke purchased coverage. This adapter follows accepted S-Q01 activation and S-Q02 occupancy, including suspended retention and confirmed-order reservation before capture.

A reservation may precede draft creation and binds a durable scoped order/version/line commitment; attachment to the intended draft/attempt consumes no second place. The per-attempt create fence is not the lifetime of the paid right. New versions/attempts require the S-U01 successor/rebind contract and cannot steal or double-allocate it. Financial compensation requires durable remedy/correction obligations; invoice posting is separate, and a draft void can require prepayment settlement despite having no service facts.

### 3.2 Consume current PriceBook providers and serve frozen bindings

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-rating-contract-con`

Current Pricing D-510 supplies `PricingReadV1::{resolve, price, current_revision}`, `PricingAcceptanceV1::{acceptance, hold}` and `SellabilityV1::{check, check_fulfilment}`. These providers do not establish a working Subscriptions adapter, remote transport or joint activation protocol.

The adapter uses authorized catalog scope and retains complete immutable AcceptedBinding and UsageRatingPolicy values. New-sale selection and acceptance occur before purchase; the supported future-purchase contract then supplies durable accepted terms and historical recovery. Subscriptions executes that evidence with current order, financial, security and readiness guards. A quote-lifetime V1 hold or live eligibility observation cannot replace durable future coverage. Exact accepted-purchase SDK mapping and integration evidence remain required.

The confirmed-order prepayment profile additionally requires durable Pricing/Products commitment to original accepted future-start/fulfillment terms **before capture**, alongside the occupancy reservation. The accepted purchase contract supplies durable future coverage; a quote-lifetime V1 hold or fresh sellability preview does not prove it. Retain the original evidence without silently repricing the purchase. Unavailable commitment, capture fencing or occupancy integration blocks this profile before Billing/Payments debit. A failed final activation preserves the paid rights for agreed remediation or explicit cancellation.

Renewal and mid-period `ends_on` resolution follow PriceBook's pin and successor rules. Automatic mid-term adoption additionally requires explicitly accepted continuation policy and satisfaction of its required notice/assent (SUB-D-41). Pricing selects the successor; price closure or a successful resolve is not customer authorization. Subscriptions publishes the authorized full successor fact and exact seal; Rating cannot adopt different terms itself. For an established obligation with a known unresolved continuation, Subscriptions publishes a full successor fact with an explicit `Unresolved` interval and its exact successor seal (SUB-D-42).

Recovery returns that exact version, including reason, evidence and preserved known lot/quantity/term provenance. Rating advances its fact head while full valuation stays pending, never a complete prefix or zero-priced tail. Resolution needs another fact/seal version; old results cannot satisfy the newer seal and publication alone does not reverse posted money. Admission prerequisites remain unchanged. Current catalog data cannot replace missing historical terms. Unsupported phases, promotional overlays or absent trial terms are refused explicitly. Detection/reconciliation operations and provider freshness evidence remain [proposed SDK work](09-consumer-contracts.md#32-consume-current-pricebook-providers-and-serve-frozen-bindings).

**Continuation observation and recovery.** Pricing publication events wake reconciliation; they do not establish accepted continuation. Recover authoritative `ends_on` and selection evidence, use known-boundary scheduling and pull recovery for missed events, and never substitute `effective_to` or a receipt timestamp for that evidence.

1. Stage Pricing's selected continuation outside the local mutation transaction. Verify accepted continuation policy and any notice/assent prerequisites.
2. At application, recheck subscription/fact heads against cancellation, increase and renewal. Commit the adopted timeline, full successor facts, exact successor seal, operation outcome and outbox together. Stale work recomputes; lost replies recover the committed outcome.
3. If an established obligation has a known uncovered boundary, publish the explicit unresolved successor. Resolve it with another fact/seal version. Unchanged accepted content needs retained validation evidence, not a new commercial revision.
4. Before first-final valuation, the selected Rating provider must prove accepted-boundary freshness. A discrepancy names the exact fact/slice and Pricing observation and requests reconciliation; the reply distinguishes validated current evidence, a published successor and pending work. A superseded fact cannot be reported as current.

The exact discrepancy/read SDK and Pricing committed-observation guarantee remain S-U08 work. `ResolvedBindings` alone supplies no snapshot proof. Reusing an observation requires proven scope and publication ordering; TTL, client receipt time and finalization delay are insufficient. Historical replay and usage corrections retain their original accepted inputs.

S-Q06 selects Pricing as trial commercial owner. The [trial evidence contract](../DESIGN.md#336-trial-terms-and-provider-evidence) places optional free-period OR credit terms on existing PlanRevision, using plan/revision identity rather than a separate TrialOffer catalog. It requires authorized exact revision reads, commercial eligibility/acceptance, frozen receipt recovery and operation-time fulfilment evidence. These are candidate capabilities, not shipped ports or new semantics for V1. The adapter preserves PlanRevision, selected variant/duration/applicability/paid-target policy and customer receipt separately from Subscriptions' actual episode.

Credit retains normal tariff bindings and references a distinct customer grant. Pricing stores no mutable balance. Products supplies SKU identity/structure only; Contracts supplies explicitly bound terms. Frozen trial/paid-target evidence survives publication changes; execution follows its explicit accepted restrictions and current lifecycle, security and financial guards. A live fulfillment observation cannot revoke or reprice those accepted terms.

The outward `bindings` read returns the accepted commercial input for the named historical period/revision. For the lot-aware profile, recovery preserves each temporal slice and its complete lot IDs, quantities, accepted-term references and purchase provenance; it cannot collapse different prices into one binding. Facts and seals return the same immutable versions as the publication path. Older fact versions cannot overwrite a newer accepted head, same-version/digest replay is a no-op, and conflicting content is explicit.

A consumer without the agreed lot-aware contract cannot enable differing-binding increases by flattening the allocation. Rating's own `pricing_snapshot_ref` identifies its calculation evidence and is a separate field/authority. The target contract keeps accepted binding identity/digest separate from Rating calculation evidence; Import remains deferred and adds no accepted-term reconstruction requirement. Matching, quantities, change effective time and commercial terms remain inspectable without making per-record usage attribution calls.

**Structured Rating pending reasons.** The consumer status contract exposes the exact requested/evaluated fact reference, accepted head, state and a nonempty list of blockers when conditions/data prevent valuation (SUB-D-44). Each blocker identifies a stable code, affected interval/lot/window/dependency (or fact-wide scope), responsible source, recoverable evidence or missing-dependency reference and expected resolution route. Multiple blockers may coexist. Rating links an `Unresolved` reason to the exact Subscriptions fact/segment evidence; its own coverage/recovery findings are durable Rating state. Human-readable text supplements machine-readable fields.

A previous complete result remains separately readable with its own exact source fact reference, never as proof of completion for the current version. Scoped SDK status reads remain available without a monetary result. Exact methods/codecs and owner adoption remain open; see the [Rating status contract](09-consumer-contracts.md#32-consume-current-pricebook-providers-and-serve-frozen-bindings).

**Empty versus zero-valued result.** Consumer/recovery contracts distinguish a complete empty replacement (no current line contributions from this fact), active zero-valued lines with accepted terms/classification/provenance, and pending status with structured reasons and no complete monetary result (SUB-D-45). Billing removes prior fact contributions only under the exact seal and its normal execution guards; it preserves other facts and history and owns invoice presentation. A zero total does not authorize erasing lines, and neither empty nor zero proves a completed refund.

Zero usage retains completeness and applicable tariff/minimum-fee requirements. Exact flag/codec mapping follows the [canonical Rating result](../DESIGN.md#rating-result-contract).

### 3.3 Recover Billing inputs without inferring invoice state

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-billing-contract-con`

See the [financial consumer boundary](../DESIGN.md#financial-consumer-boundary).

`groups_due` discovers groups that no event announced. `facts` retrieves the group, all referenced fact versions and its requested composition seal, including explicit zero obligations. `period_facts` also discovers open periods before group due time, which is required for usage scheduling.

Billing validates immutable payer/group/fact relationships and consumes Rating's full parent-fact results. A commercial seal does not prove complete hourly results. Subscriptions never marks financial posting complete from event delivery, writes a credit amount or uses a late current payer lookup. An authoritative Billing watermark/period-state read is a separate dependency for any enabled backdating guard; absence fails closed.

S-Q03 routes the amount-less qualifying fact to Rating for frozen-term tariff valuation; Billing accounts/posts, applies allocated advances and owns final invoice rounding/corrections. Taxes/payments retain their owners. S-Q04 selects durable identity per separately agreed chargeable action. Retry/new transport key/attempt/rebuild is the same purchase; resume or a price change creates no fee. A new connection purchase after cancellation qualifies only under explicit new agreed terms. Adapters cannot infer a charge from a component interval, phase, reservation or prepayment. Corrections retain the same occurrence with versions.

Import/adoption is deferred and adds no current fixture or lineage-mapping requirement. Exact schema, identity conflict handling, routing and binding/calculation-snapshot/replay contracts remain S-U08/S-U13 gates. S-U01 rebinding and S-Q02 protected rights remain intact. Fee-bearing capture/activation stays gated on contracts and runtime fixtures, with pre-capture protection unchanged.

The [financial compatibility matrix](08-commercial-facts.md#36-seal-the-complete-commercial-set) defines the proposed minimum binding/result, occurrence, seal, advance and posted-period evidence. Adapters retain source and consumer versions separately, recover a complete target at a consistent barrier and never infer invoice posting from publication. Administrative backdating is deferred. Ordinary full-result corrections retain the S-U13 financial consumer guarantees.

### 3.4 Consume versioned Contracts terms

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-contracts-contract-con`

See [RO-2026-10-15](04-renewal-retention.md#7-renewal-and-offboarding-integration-contract).

Retain the accepted Contracts version and effective period for term, renewal and agreed grace-policy inputs. Renewal, grace, suspension dwell and retention/archive use versioned configuration policy, with explicitly bound agreed terms taking precedence. Collection pause and notices are deferred; an accepted term that requires an unavailable notice keeps the dependent action blocked. Snapshot selected values/provenance and evaluated deadlines; configuration changes cannot rewrite a running deadline.

The 7/90/30-day values are the accepted v1 platform defaults; when a bound Contract ends mid-term the running term completes unchanged and the next renewal binds to an explicitly bound successor Contract, otherwise to the platform defaults subject to the deferred notice prerequisite. An event may prompt a read. It must not replace missing authoritative fields with assumptions. Commercial overrides and ramps require explicit compatible owner contracts. Unsupported commercial overlays refuse before effects.

Billing is the accepted owner of promotional trial grants/history/balance/allocation/correction. The [S-U15 provider contract](06-trials.md#credit-preparation-and-recovery) defines explicit Orders business-origin identity, staged coverage preparation and same-grant finalization from authoritative applied T, immutable policy/scope/original-expiry evidence, authorized versioned read/outcomes and full-result allocation with prior-version guards. Exact wire/schema, mirrored owner ratification and implementation remain gates; the contract is not a delivered API. For initial credit access, Billing commits the reserved grant before Entitlements establishes usable readiness.

Subscriptions carries the commitment reference/digest in preparation posture and records it with actual T, original episode expiry and recording outbox at activation. Billing's guarantee makes that same grant effective at T; later materialization records it without a new decision or initial-access gate. Pre-T preparation is platform-borne without bonus drawdown. Missing outcomes require exact activation recovery; only fenced non-activation permits release, while later cancellation remains authoritative. Orders uses order/version/line; standalone acquisition is deferred.

Fenced non-application settles aborted preparation, while unknown/applied outcomes recover their existing identity and history. Late finalization never resets T/expiry or restores expired access. Exact provider/fence/scope ratification and runtime proof remain required; no remote Billing call occurs inside the local transaction. Consumers must distinguish active unconverted-trial expiry/exhaustion from retained-grant outcomes after paid conversion. After early conversion has already applied, retained-grant expiry/exhaustion ends only bonus coverage; it does not itself stop or revoke paid service, duplicate conversion or reverse it.

Remaining paid expenses follow the accepted paid-plan/settlement rules and applicable guards; no unsupported liability or waived payment prerequisite is inferred. Consumers recover the same grant on retry and share it across eligible items once. Billing accepts canonical full Rating replacements, preserves prior allocation/version lineage and adjusts the same grant under original scope/currency/terms; duplicate accepted versions are no-ops and conflicts refuse/recover. Independent delta streams, duplicate grants/drawdowns/restorations/invoice credits and inline rewrites of posted records are not allowed.

Valid/applicable-grant correction may restore availability for an active trial or eligible paid usage after early guarded conversion, only until the same original expiry. An early valid guarded paid transition retains unused credit on the same grant for eligible paid-plan expenses until its original expiration, with unchanged currency/product/item/charge-kind restrictions. Closing the trial by this conversion does not revoke that eligibility or extend the grant; balance alone grants no service permission.

Billing applies the same full-result correction lineage to eligible paid usage; retained credit cannot satisfy upfront-payment guards unless the agreed coverage/settlement contract allows it. After grant expiry, Billing corrects history/accounting with zero spend permission and unchanged expiration; no trial extension or access revival follows. A later correction cannot automatically reopen a trial already ended by exhaustion, even before original expiry: restored accounting balance grants no service/spend permission for that closed episode and does not reverse an applied paid conversion.

V1 has no reactivation route for a closed trial episode. A new acquisition is a fresh paid purchase through Orders; repeat trials remain deferred even with explicit policy. Paid acquisition and financial admission retain their S-U01/S-U07 gates. Billing draft/posted correction uses the agreed Ledger posting path; current cash-funded `post_credit_application` is not a delivered free-bonus API. An unknown balance means neither zero balance nor permission to spend. Subscriptions consumes evidence references, not raw metering or balance math.

Exhaustion/expiry cannot authorize paid service, create customer debt from a stale event or revive a closed episode after correction. For eligible accepted-trial expenses served before the authoritative applied paid boundary, the platform bears full rated value beyond bounded bonus coverage, including positive corrections, errors and accounting/stopping-delay or in-flight overrun; it creates no customer trial-overage debt. Usage actually served after a valid guarded paid transition follows accepted paid-plan/settlement terms after eligible bonus coverage.

Classify by served time and the applied boundary, never rating/event/correction arrival; split crossing usage using supported evidence and never backdate paid conversion. Billing must distinguish bounded bonus coverage, platform-borne amount and customer obligation while Rating preserves gross usage/value. Provider evidence must support delayed and boundary-crossing facts without backdating or allocation by arrival time; erroneous billed trial tail follows existing idempotent correction paths. Exact provider/Ledger/evidence, detection/enforcement/recovery and other liability cases still gate launch, without new journal/refund schema or unlimited bonus/free operation.

Existing paid-wallet ownership and postpay credit-service permission remain separate.

Trial extension/repeat/additional-bonus/transfer/stacking are unavailable in the current profile even with explicit Pricing permission. Consumers preserve initial eligibility evidence and the payer/plan admission fence, original episode/grant identity and frozen terms. Early paid carryover remains required; no rollover/regrant/reactivation or terminal revival is enabled. Financial and authorization guards remain independent.

For this proposed provider, persist grant/operation/result/allocation references and consume explicit applied, replayed, pending, conflict, stale/gap and financial-unknown outcomes. Recover lost preparation/finalization by its durable Orders acquisition/benefit identity; another transport key or attempt cannot create another benefit. A positive balance does not authorize access or replace applicable settlement guards. Billing serializes shared-grant allocation with the canonical full Rating replacement and prior version, then recovers separately recorded invoice/Ledger work; Subscriptions does not calculate or repeat that financial effect.

Reordered outcomes must be reconciled with the current lifecycle revision, and a pending allocation or remote posting must not be advertised as complete. Eligible pre-boundary trial positive corrections apply the accepted platform-liability rule; unsupported correction/policy cases outside that scope remain explicit gates. Joint fixtures must cover concurrent drawdown, source conflicts/gaps and crash-after-financial-effect recovery as specified in the companion.

### 3.5 Separate policy, materialization and resource evidence

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-policy-oss-contract-con`

For initial activation, the [assignment readiness contract](05-entitlements.md#initial-activation-readiness-contract) binds Entitlements-authenticated aggregate results by ordering tenant, scope, operation, posture revision/digest and manifest identity/version/digest to the saved activation operation; adapters need not know subscription identity (SUB-D-55). Persist the operation and posture before dispatch, reconcile unknown/missing observations, and fence both late ready and late external effects after cancellation. SUB-D-57 selects the held-readiness protocol: operation recovery returns the immutable applied or irrevocably non-activated decision for the exact preparation generation and commitment; a pending, missing or inaccessible result cannot authorize cleanup.

Exact owner SDK mapping and race fixtures remain required. License Resolver checks rights; issuance and change feed are separate owner capabilities.

Stage policy/eligibility checks before the commit and recheck their local applicability at commit. Denial or dependency unavailability does not grant resource access. Entitlements materializes Subscriptions' commercial entitlement posture; the platform enforces access and resource admission. Resource readiness/deletion and historical inventory are authoritative source inputs, not confirmation inferred from a subscription status or an elapsed retention timer.

No-match and ambiguous attribution have explicit operator-visible states. The selected orphan action requires the platform's authorization and owner decision. Historical commercial ownership survives cancellation for replay. Accepted S-Q05 ends ordinary customer tariff at the explicit customer-confirmed cancellation charging cutoff despite platform stop delay, and after authoritative cancellation/offboarding stop. A future scheduled cutoff permits normal service before then under existing terms; request receipt/acceptance for processing is not cutoff confirmation. This leaves suspension bill/waive policy unchanged.

Retained records/resources, export, hold or delayed deletion alone create no charge; explicit agreed retention terms, including existing terms, may support their own obligation.

S-U05/S-U13 need a versioned cross-owner contract separating confirmed financial commitment from eventual guarded commercial/resource completion, not an assumed current API. Preserve authoritative correlated actual stop, callback time, financial cutoff and deletion evidence separately. A late worker/callback cannot shift a known cutoff or fabricate an earlier physical stop; unknown stop may block final closeout but never justify customer tariff beyond that cutoff. A financial boundary alone never releases occupancy or authorizes deletion/archive.

Customer cancellation is confirmed only after authorization, commercial eligibility and durable recording of its charging cutoff; confirmation is independent of resource provider completion. Withdrawal or rescheduling requires an explicit versioned confirmed action before the applicable cutoff, without retroactively restoring charges. After cutoff, new service needs a separate valid guarded route, not undo of the financial commitment.

Exact confirmation/evidence, capture/compensation/state and correction fences remain open; internal stops are split by cause (a platform-fault or maintenance stop beyond the accepted SLA ends accrual at the authoritative stop with Billing correction; an operator, abuse or policy stop follows suspension bill/waive policy without creating a cutoff).

Resource provider/usage owners retain actual runtime/usage; Subscriptions retains relevant evidence/lineage and the nonchargeable technical extent with frozen policy under the agreed contract, without owning raw-usage ingestion/storage. Late pre-cutoff reports preserve original coordinates, attribution, frozen terms and versions. Rating values corrected obligations; Billing corrects an erroneous charge/posting and settles overpayment idempotently, preserving allocated advances. Legitimate prior debt/commitments and explicit retention obligations survive; erroneous tail charges cannot be shielded as prior debt.

No usage deletion, zero catalog price, collection-pause waiver, extra goodwill payout or cash-refund method/timing/credit formula is inferred. Retention safeguards and paid-order remedies remain unchanged; S-U05/S-U13 contracts/runtime are still gates.

The [renewal and retention provider matrices](04-renewal-retention.md) specify proposed confirmation revisions, pre-cutoff supersession, resource provider effective-stop evidence, financial acknowledgements and hold/deletion generations. Adapters must preserve expected source generation through the owner request and its application acknowledgement, not just compare a late callback. Recover unknown work by the original operation; receipt, physical completion and invoice/correction completion remain distinct. The adapter carries the stop cause so the accepted cause-split rule can be applied; it infers no remedy amount.

### 3.6 Consume payments and trigger notices

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-algo-payments-notif-contract-con`

Payments/Billing and the designated credit/terms owners supply authoritative evidence appropriate to the agreed settlement scheme. For S-Q06 prepay, the prescribed payment must be satisfied before paid conversion; a stored method is not payment success. For postpay, accepted terms and credit-service authorization suffice without payment/card at conversion; ordinary paid accrual starts then and invoice/payment follow the agreed due date. Delinquency begins only once payment is due and unpaid, then follows the agreed recorded failure/retry/restriction policy. Detailed postpay due/dunning contracts remain OPEN.

Definitive prescribed-prepay failure at trial end defaults to no paid start, recorded failure/retry and normal expiry with existing data retention; free continuation requires explicit bounded trial terms. Trial extension is deferred. Supported retry and later paid acquisition require the AP/FA provider mappings, without reopening a closed trial.

No applicable authorized settlement route means normal trial expiry/revocation with existing data retention and no debt/setup charge from expiry alone; valid authorized postpay obligations are not waived. Relevant unknown/outage is not absence or permission. Evidence follows the selected route, so invoice/postpay does not gain an unrelated card-provider dependency. Unknown payment is not definitive failure/absence/success: recover the original durable result before retrying collection. Retries must not duplicate collection, and uncertainty must not release protected prepaid claims.

Later successful payment supports only a valid guarded paid transition at its actual boundary, with no waiting-gap accrual/debt or instant Entitlements guarantee. Closed trial episodes and terminal cancelled subscriptions cannot be revived; exact later paid acquisition/Orders association remains gated under S-U01/S-U07/S-U12 as applicable. No new trial/setup charge is inferred. This approval auto-migrates no existing/prepaid customer to credit and forgives no prior debt. Renewal/grace/trial features apply outcomes through ordinary lifecycle guards; Entitlements remains asynchronous. Notification scheduling/delivery is deferred.

Required unavailable notice still blocks its dependent action; ordinary domain events remain for future consumers.

For a confirmed future-start order, payment authorization is not actual capture. Any option tolerating authorization failure cannot waive durable place and commercial commitment before debit. Capture must use the owner-agreed confirmation/cancellation fence; timeout or unknown result requires durable financial recovery, never treating the reservation as unpaid and releasing it. Cancellation fences late capture and activation and records the applicable financial-remedy obligation. Financial policy and release/settlement ordering remain owner work; no unconditional refund or universal wait for a posted refund is inferred.

The [capture/cancellation protocol](01-foundation.md#acquisition-recovery) assigns Orders-origin arbitration to Orders Lifecycle. Require the winning durable acquisition/version decision and dispatch identity, then consume Payments outcomes by that same capture identity. Cancellation before start fences delayed debit; cancellation after start may leave financial recovery/remedy pending and must fence activation independently. Unknown capture retains the claim and original-term protection. Release requires the coordinated owner acknowledgements, authoritative financial resolution and any required recorded remedy; neither receipt of cancellation nor draft void is sufficient.

Payments must durably suppress late/duplicate execution or recover its actual outcome, not rely on an earlier caller read. Standalone acquisitions are deferred; authorized postpay without immediate debit does not enter this capture protocol. Exact SDK/provider contracts remain gated.

For renewal, request the policy evaluation and source evidence described in [feature 04](04-renewal-retention.md): explicit term/default versions and precedence, settlement scheme, renewal identity and due/credit/payment status. Return or retain typed incomplete-policy, payment-unknown, credit-authority-unavailable, notice-capability-unavailable and source-version-conflict outcomes. A method or authorization is not captured prepayment; authorized postpay is not delinquent before due. Collection pause and notice payload/version retention are deferred; the adapter carries neither in this version. Contract events are recorded and interpreted under explicit binding/termination policy; they do not create subscriptions, retroactively move deadlines or silently select defaults.

### 3.7 Consume product identity and occupancy policy

- [ ] `p2` - **ID**: `cpt-cf-bss-subscriptions-algo-registry-contract-con`

Products owns SKU identity/version and the agreed overlap-scope classification; Pricing owns accepted plan/revision/item and price bindings. Accepted S-Q02 occupancy uses `(payerTenantId, resourceTenantId, overlapScopeKey)` with explicit `maxConcurrentActive` and versioned provenance. Missing key/limit evidence refuses admission; the adapter neither defaults to one nor derives classification from display names or example products.

Report actual active subscriptions, retained suspended places and explicit order reservations separately. They consume the same admission capacity, each right once. Held payment-pending reservations count. Active 0 plus either retained 1 or order-reserved 1 at limit 1 leaves no free place. Confirmation reserves before capture; confirmed-order activation atomically converts that same reservation to active without competition or another place. Other supported unreserved paths require ordinary atomic admission and cannot bypass pre-capture safeguards if collecting upfront. Suspend and unchanged-key resume retain/convert the same claim.

Active/suspended cancellation releases at application; pending/failed cancel or TTL/dwell expiry does not. Order-reservation cancellation additionally follows the agreed capture/consumption fence and financial-remedy protocol. A versioned limit decrease preserves existing protected active, retained-suspended and order-reserved claims; only new admissions that would exceed the current limit refuse. Own-reservation activation and unchanged-key resume keep the protected right and other guards. Policy/key/claim movement requires a destination/version mapping and atomic move; unsupported or failed movement preserves the source claim and refuses.

Claim evidence is not proof of payment, Policy, term, resource or usable Entitlements access. Initial activation additionally waits for confirmed usable readiness; resume and ongoing entitlement delivery retain their asynchronous contracts.

The current Orders `activeCount`-only read and release-on-leaving-active recommendation cannot enforce the selected rule. S-Q02/S-U10 require versioned evidence for actual active, retained suspended and explicit order reservations with common key/limit provenance, order binding and aligned release/consumption semantics. Do not relabel suspended/reserved as active or reinterpret an old observation as sufficient evidence. No new wire API name or implemented provider is asserted. Producers, consumers, future commercial commitment and capture fencing must align before the prepayment profile is enabled.

Plain unconfirmed draft state and availability reads create no reservation. A paid confirmed reservation survives future-date waits, holds, ordinary TTL and technical fulfillment failures until activation or explicit cancellation under the agreed remedy protocol. Unknown capture also retains the right pending financial recovery; unpaid-abandoned cleanup cannot steal it. Linked subscription replacement is deferred; future Orders bookings still count from confirmation.

Detailed reservation/financial contracts and exact policy/key movement mappings remain OPEN; unsupported current key changes cannot erase a claim or promise unrestricted future eligibility; legacy migration is excluded. Historical Pricing retirement/presence-call assumptions are not active requirements for the delivered PriceBook provider.

### 3.8 Recover gaps, cursor loss and retained history

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-recovery-cursors`

All recovery reads bind authentication, authorization scope, query/range, visibility boundary and continuation. Revoked access is checked on each request; a cursor cannot retain revoked privileges. Expanding a consumer's grant requires a fresh bootstrap of the expanded scope, not continuation of an old filtered cursor.

`segments_since` returns an ordered, contiguous retained sequence prefix for a resource tenant, with the producer high-watermark and explicit continuation/completion. A gap never advances the consumer's applied checkpoint. A cursor/sequence older than retained history returns a typed retention-loss outcome with the earliest available position and affected known scope, not an empty successful page.

The producer must retain immutable, recoverable history longer than the broker retains events. If bootstrap from a complete historical snapshot is offered, the snapshot carries a sequence barrier and its full scope; the consumer installs it atomically before continuing beyond the barrier. If such a snapshot or required historical interval cannot be proved complete, persist a source-loss exception, keep dependent proofs pending and require authoritative repair. Resetting to the oldest available position alone does not prove recovery.

Replay retention must exceed the largest enabled finalization delay plus the supported outage/replay window. Financially referenced versions, scopes and correction lineage survive operational data purge. Concrete duration, paging limits and service-identity read grants are release gates in S-Q07/UPSTREAM_REQS.

**Subscriptions-owned recovery implementation profile.** This package specifies the local mechanism for the already-required event-independent recovery and immutable history. It adds no new billing policy or Change Orders behavior. Semantic field names are design notation, not shipped SDK methods. Owner-consumer wire agreement and runtime proof remain S-U11 and the applicable financial/attribution gates.

1. **Commit-ordered recovery journal.** Append an immutable recovery record identifying changed fact/group/seal or attribution versions in the same local transaction as those versions, their source mutation and outbox. The partition identity includes stream kind and its declared tenant axis; the attribution stream remains per resource tenant, distinct from commercial ordering and resource provider lifecycle sequences. Consumers retain a vector of partition checkpoints, never compare unrelated sequences as one clock.

   Allocate each partition position through a transactionally updated, locked head row; hold the lock until commit/rollback and lock multiple heads in deterministic order. A committed high-watermark has no lower position that an unfinished transaction may later make visible. A nontransactional sequence allocation, timestamp or broker offset is not this completeness barrier. This profile needs backend/concurrency and throughput validation; no measured scalability claim is made.
2. **Stable snapshot without a transaction held across client pages.** On bootstrap retain a snapshot descriptor: authenticated consumer/scope identity, canonical query/range and contract version, immutable committed partition barrier vector, and explicit validity/retention bounds. Use append-only head/membership history to reconstruct the latest version at or below each barrier. Evaluate filters and membership against that historical state, not the current mutable heads; use a stable unique business-key order and keyset continuation. Each continuation is opaque and integrity-protected and binds the descriptor plus last returned key.

   Each page reauthorizes the same scope. A cursor never grants access. An unsupported historical query is refused. It cannot mix old and new pages. The vector proves the declared per-partition view, not a globally simultaneous distributed snapshot; exact immutable cross-references and seals remain necessary for financial readiness.
3. **Separate discovery from change catch-up.** `period_facts` and `groups_due` discover their declared snapshots; an immutable commercial change journal additionally exposes every later affected fact/group/seal version by commit position, including corrections to an old or already-due period. Bootstrap returns the matching continuation barriers; subsequent catch-up starts strictly after those barriers and fixes a new committed upper boundary for each pass. Entries carry immutable version/digest references and explicit supersession/zero-obligation state where applicable. A due-date cursor alone cannot find a correction committed today for last month's period.

   Physical deletion must not erase a required recovery outcome or financially referenced history. Exact outward commercial catch-up encoding is an SDK/consumer delivery task; until supported, due-date-only polling cannot be advertised as complete event-independent recovery.
4. **Consumer checkpoint and replay.** A page reports the covered source interval, fixed upper boundary, continuation and whether that requested interval has been exhausted. Query filtering is explicit: absence of matching records is not an unexplained source gap or proof of zero usage. A consumer validates scope, continuity and version/digest rules, stages required referenced records, and atomically persists its local accepted projection/inbox and checkpoint. An unfinished batch or missing referenced version cannot advance the applied checkpoint past the missing work.

   A lost response replays the same immutable records; old versions cannot replace newer heads. This checkpoint proves ingestion, not a posted invoice or applied remote effect. Consumer financial work retains its own durable idempotent execution/recovery; Subscriptions supplies evidence rather than committing consumer state.
5. **Cursor expiry versus history loss.** A snapshot descriptor expiring while authoritative history remains available requires a fresh authorized bootstrap; it is not automatically source-data loss. A journal position older than retained history returns a distinct retention-loss result and the authorized available bounds. A fresh snapshot is an adequate repair only if it covers the full required historical scope and version/obligation lineage, including corrected and zero obligations; a current active-subscription list is not adequate.

   Stage complete bootstrap pages under a new local snapshot generation and atomically switch the consumer's installed generation/checkpoint only after validation. Retain the previous installed state during incomplete bootstrap. If complete history cannot be proved, persist an explicit repair-required outcome. Keep affected completeness/finality pending. Never reset to the oldest surviving cursor as if the gap had been processed.
6. **Retention and authorization are enforced dependencies.** Retain versions/head history needed by valid snapshot descriptors through their supported validity, as well as independently required financial/correction history. Operational compaction may expire a descriptor only through the documented typed outcome. It cannot silently change what an existing page means. Select retention/limits from the configured operating profile and prove coverage of finalization plus supported outage/replay; this package invents no duration.

   Authorization loss terminates continuation; an expanded or changed query/scope requires bootstrap and cannot reuse an old filtered checkpoint as evidence for the new scope. Return retained bounds or diagnostics only within current authorization.

**Recovery-package fixtures:** writer A allocates a position and stalls while writer B tries to commit on the same partition; A rolls back without a phantom gap; a correction/zero obligation is committed between snapshot pages; last month's fact changes after a due-date scan; a mutation commits but every event is lost; a consumer crashes after staging a page but before checkpoint commit; referenced immutable data is unavailable; snapshot expiry with intact history differs from actual retained-history loss; authorization is revoked or scope expands mid-read; and bootstrap fails before generation installation.

Verify exact barrier continuity, unchanged page membership, discoverability of late corrections, no checkpoint beyond missing work and no duplicate financial execution. These are design acceptance cases, not executed tests.

**Rating output recovery.** The selected provider exposes exact immutable result/manifest reads, exact-fact work/head status and authorized snapshots plus committed catch-up for result changes. The same guarantees above apply: fixed membership, explicit completion, old-period correction discovery, atomic applied checkpoint advancement and distinct snapshot-expiry/history-loss outcomes. Outbox claim order alone proves no committed barrier. Producer implementation may choose any protocol that demonstrates these guarantees.

Missing dependencies are durably staged, without a complete monetary result. Status blockers distinguish unsupported capability, invalid precision/range, identity/digest conflict, missing predecessor/terms, unresolved continuation, unsupported allocation/boundary split, missing coverage and history loss. Authorization denial and provider unavailability retain their canonical error semantics. They do not mean empty scope or zero money.

An unsupported profile refuses before new capture, activation or increase. Capability loss after an accepted obligation preserves visible pending/remediation work under that obligation; it does not flatten lots, discard charges, select a different tariff or authorize another collection.

### 3.9 Replace attribution without erasing financial history

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-attribution-replacement`

Segment identity/version/digest is immutable; updates publish complete replacement state. `segments_stream_seq` is the attribution position: one sequence per resource tenant, independent of the subscription ordering tenant. The provider resource lifecycle sequence retained with each event is not an attribution position and is never compared with `segments_stream_seq`; the original resource lifecycle barrier is retained separately.

Deletion closes an existing historical interval rather than deleting its identity. A correction uses a higher version plus the affected old/new interval and fact lineage. Same version with different payload is a conflict. Rating may re-attribute contributions at their stored coordinates and create replacement financial results; neither source nor consumer rewrites a posted invoice.

Attribution lookup outcomes are matched, no-match, ambiguous, or incomplete due to an unresolved source gap. Recovery never guesses a match. A usage interval crossing an ownership boundary requires the usage producer's agreed splitting/normalization contract; Subscriptions does not fabricate fractional usage. Source sequence gaps, unsupported boundaries and ambiguous ownership block sealing of the affected Subscriptions ownership scopes until repaired.

**Proposed provider proof and reconciliation matrix: S-U02/S-U03/S-U04/S-U11**

Each proposed request binds authenticated tenant/consumer scope, stable operation or query identity, exact historical range and required source versions. Each response names its source authority, immutable barrier/revision, covered scope/range, complete/partial/unknown disposition, digest and recovery continuation where applicable. These semantic requirements await provider ratification and implementation. An existing resource endpoint or transport acknowledgement does not establish them.

For entitlement and resource execution rows, this matrix names the evidence Subscriptions needs, not a required native response DTO or endpoint. Entitlements owns the provider-independent boundary. The selected adapter can obtain and normalize equivalent authoritative evidence. Inventory and usage-evidence reads remain Subscriptions consumer contracts with the selected evidence providers. Provider-specific status names, sequence/snapshot representations and API mappings belong to each integration's specification.

Required evidence cannot be inferred from an acknowledgement, a cache or the mere presence of an adapter; unsupported profiles remain explicit without making a specific license/resource provider mandatory.

| Provider / request | Required proof and accepted outcome | Refusal/recovery and fixture |
| --- | --- | --- |
| Historical inventory / resources for a fact window | Fact/version, resource tenant/window and inventory query; owner snapshot identity plus lifecycle barrier, complete expected resources/usage-type versions and effective intervals | Empty is valid only with complete authoritative inventory evidence. Unknown snapshot, missing lifecycle range or pagination gap stays pending; a current resource list is not historical proof. V07/V09 cover proven empty, deleted resources and delayed readiness |
| Usage emitter and collector / coverage for the same scope | Emitter's declared interval/source-sequence completeness independently of inventory; collector's durable recorded-through evidence and explicit gaps; Rating combines them with the Subscriptions ownership proof | Ownership alone proves neither emitted nor ingested usage. Missing emitter or collector coverage blocks Rating usage completeness and dependent Billing financial finality, even with no samples; it does not block an otherwise complete Subscriptions ownership scope or commercial membership seal. Contradictory sequence/range is an explicit gap requiring owner recovery, never synthetic zero. V07/V10 lose one hourly source segment |
| Entitlement fulfillment adapter / recovery | Subscription-owned desired revision, scope/effective boundary, digest and stable operation reference; adapter-normalized evidence of pending, applied or failed external effect with provenance | Vendor states/fields are mapped in the adapter specification. The adapter/provider integration must prevent stale issue from undoing newer revoke and recover actual application after timeout; its dispatch log alone is insufficient. Initial credit access uses the authenticated operation-bound Billing commitment and held readiness; later Billing recording is not an access gate. Pre-T use is platform-borne and no bonus is consumed before T. V11 tests the abstract guarantee for the selected integration |
| Authorized range/read continuation | Fixed query/scope and immutable read barrier, requested version/range, per-page progress and producer high-watermark; continuation bound to the same authorization/query/barrier | Scope change requires new bootstrap; every page rechecks authorization. Cursor loss reports earliest retained position/affected range, never empty success. Install a proven complete snapshot before advancing its checkpoint; no missing-history skip. V10 denies access mid-page and resumes after retention loss |
| Subscriptions ownership-scope proof | Exact fact/version/window, authoritative inventory snapshot/lifecycle barrier and applied attribution sequence; expected resource/usage-type intervals and digest | Subscriptions seals only this ownership proof once its required inventory/attribution evidence is complete. Unknown inventory stays pending, never guessed empty. Emitter/collector coverage is not a prerequisite for this proof; V07/V09/V10 retain these independent outcomes |
| Subscriptions commercial membership seal | Owned BillingGroup/composition version, complete exact `(fact_id, fact_version, payload_digest)` member references and set digest, as specified in feature 08 | Seal the owned complete fact set independently of usage/Rating completeness. Missing owned member/version blocks this membership seal; missing emitter coverage alone does not. A newer fact needs its successor seal; a new calculation result for the same exact fact does not. This seal proves neither ownership scope nor rated/financial completeness |
| Rating usage completeness | Subscriptions ownership-scope proof plus independent emitter interval/sequence completeness and collector recorded-through/gap evidence for the exact window; canonical result/window versions and required child manifest | Rating joins its required inputs and keeps usage completeness pending when any is missing. No joined raw-usage proof is assigned to Subscriptions. V07/V10 show a complete Subscriptions scope/membership set while Rating remains incomplete for missing emitter evidence |
| Billing financial finality | Required commercial membership seal and complete canonical Rating results/manifest with their source versions, plus Billing's financial target/invoice guards | Billing alone determines financial readiness from both required inputs; either missing input blocks that finality. An ownership proof or membership seal alone cannot substitute for Rating completeness. V07/V10 retain the separate producer outcomes during recovery |

The tables assign no raw metering, quota counters or source-inventory certification to Subscriptions. Local inbox/outbox and source checkpoints are atomic with their accepted projections; remote effects are recovered by stable identity and owner-applied version. Replayed lower revisions cannot regress access, attribution, adoption or financial finality. The proposed proof profile refuses unsupported boundary splitting and retained-history loss rather than silently broadening an owner's guarantee.

**Parameterized operating and recovery evidence plan**

This is a benchmark/failure-test design, not an execution report or SLA. Use [PRD §7](../PRD.md#7-non-functional-requirements) as the versioned target profile ratified as product target v1 on 2026-10-08, and record SRE/consumer measurability confirmations under S-U11. No duration, capacity or recovery guarantee is measured here.

The proposed evidence run records the selected contract/build versions, hardware/topology, provider availability and workload parameters: subscription/tenant volume and skew, concurrent intents, facts/windows/items, renewal burst, replay mix and retained history. Compare measured outcome counts, latency/publication/issuance distributions, queue ages, saturation and visible overload/pending outcomes with the explicitly selected PRD profile; missing load assumptions or unmeasured parameters prevent a capacity/SLA claim. No rejected or pending operation may be omitted to improve a latency figure.

**Failure/recovery scenarios:** V01/V05/V10/V11/V16 inject crash before/after commit or remote effect, loss/reordering/duplication, missed period/seal, provider outage and cursor loss. Preserve independent expected identities/outcomes; record recovered heads, oldest pending age and time to safe reconciliation. Restore state/receipts/tombstones, immutable facts/seals/attribution, source barriers and pending effects, then reconcile source to financial facts before claiming safe recovery. Report actual lost/restored intervals against separately ratified process-failure and disaster RPO/RTO assumptions.

Check that source retention and complete-snapshot capability cover enabled finalization delay plus supported outage/replay; missing coverage disables that profile rather than silently shortening history. The proof record contains expected/observed outcomes, measured distributions, unresolved findings and owner acceptance references. Mock runs establish contract intent only; implemented joint runs establish runtime proof, and V17 remains the separate traceability check.

## 4. States (CDSL)

### 4.1 Scope proof readiness

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-state-scope-readiness`

**States**: `pending`, `sealed` for a scope version.

**Transitions**: a requested scope is pending until inventory and attribution barriers are reconciled and the requested range is complete. Sealing stores immutable evidence. Later attribution corrections create a new scope version and invalidate dependent current projections through an explicit versioned outcome; the prior proof remains readable for audit. A broker acknowledgment never seals a scope.

### 4.2 Recovery exceptions

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-state-recovery-exception`

**States**: `open`, `repaired`, `resolved_by_authorized_policy` for an auditable exception.

**Transitions**: a detected gap/loss/ambiguity opens an exception. Authoritative restored evidence closes it as repaired. A separately agreed and authorized commercial disposition may resolve an exception without pretending lost evidence was recovered; any resulting write-off or adjustment retains its reason and lineage. Time passage, an empty response or operator acknowledgment alone cannot assert completeness.

## 5. Definitions of Done

### 5.1 Reads are sufficient without events

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-dod-recovery-sufficient`

The system **MUST** expose authenticated, consistent and bounded discovery/recovery of historical bindings, open and closed periods, groups, facts, seals, segments and scopes, with explicit retention-loss and partial-range outcomes.

**Availability and restoration evidence:** retain an operational recovery inventory and an exercised backup/restore procedure covering commercial state, receipts/fences/tombstones, immutable input/history, fact/seal/attribution versions, source checkpoints and pending delivery/effects. Test process restart and storage failover with durable storage intact for the proposed RPO 0 acknowledged-commit objective; separately run disaster restoration against the proposed RPO ≤ 15 min / RTO ≤ 60 min and follow it with source-to-fact reconciliation. Record actual lost/restored intervals and time to safe service recovery; missing evidence remains pending rather than being fabricated to meet the clock.

The ratified availability target is 99.9% monthly for configured control/recovery surfaces. Before declaring readiness, S-Q07 must ratify the SLI definition, eligible surfaces/requests or time intervals, dependency/failure scope and budget accounting, and the runbook must provide measurements for that definition. A successful restore drill does not prove a month of availability. Disaster recovery and storage-intact process recovery are distinct scopes; restoration must not discard any surviving operation receipt or business fence. All objectives are ratified v1 targets from [PRD §7](../PRD.md#7-non-functional-requirements), not measured claims.

**Implements**: `cpt-cf-bss-subscriptions-flow-boundary-con`, `cpt-cf-bss-subscriptions-algo-recovery-cursors`, `cpt-cf-bss-subscriptions-algo-rating-contract-con`, `cpt-cf-bss-subscriptions-algo-billing-contract-con`.

**Touches**: `SubscriptionBillingReadV1`, authorization, immutable stores, recovery pagination and consumer contract fixtures.

### 5.2 Authoritative attribution and bounded proof production

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-dod-attribution-proofs`

The system **MUST** publish gap-recoverable full-state attribution and produce batched ownership proofs from historical inventory barriers, keeping scope distinct from usage coverage and retaining ambiguity/source-loss evidence.

**Capacity evidence:** use PRD §7's ratified workload: 100,000 active subscriptions in one large tenant, one million across ten tenants, one steady hour at 100 mutations/s and 1,000 authorized reads/s followed by a five-minute twice-rate burst. Measure cold projection rebuild, hot-key behavior and one logical per-fact batch covering all 744 windows of a 31-day UTC hourly parent, with bounded snapshot-consistent pagination where required.

Record resource cardinality, component distribution, proof size, paging limits, hardware/storage and external-provider timing; do not extrapolate an unbounded resource/tenant promise from a small fixture. Measured evidence under S-U11 precedes production scale claims.

**Implements**: `cpt-cf-bss-subscriptions-flow-publish-attribution`, `cpt-cf-bss-subscriptions-flow-scope-batch`, `cpt-cf-bss-subscriptions-algo-attribution-replacement`, `cpt-cf-bss-subscriptions-state-scope-readiness`, `cpt-cf-bss-subscriptions-state-recovery-exception`.

**Touches**: lifecycle-source inbox, attribution log, inventory reconciliation, scope/version stores and event outbox. Inventory/coverage owner dependencies remain release gates; a fake provider demonstrates contract behavior only.

### 5.3 Boundary adapters preserve their owner's decisions

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-dod-consumer-adapters`

The system **MUST** retain caller scope, operation identity, frozen commercial inputs and external evidence without taking over the provider's authority or enabling unsupported semantics.

**Implements**: `cpt-cf-bss-subscriptions-algo-control-plane-con`, `cpt-cf-bss-subscriptions-algo-contracts-contract-con`, `cpt-cf-bss-subscriptions-algo-policy-oss-contract-con`, `cpt-cf-bss-subscriptions-algo-payments-notif-contract-con`, `cpt-cf-bss-subscriptions-algo-registry-contract-con`.

**Touches**: lifecycle adapters, current Pricing SDK ports, Contracts/Payments/Entitlements/Policy/inventory integration fixtures and capability gates.

## 6. Acceptance Criteria

- [ ] With all events dropped beyond broker retention, Billing discovers a missed due group and Rating discovers its open/closed periods, fact versions and bindings through reads. Both ingest the same business identities as event delivery.
- [ ] Concurrent new facts during pagination do not cause skipped or duplicated members of the pinned range. Partial pages cannot claim complete commercial or ownership evidence.
- [ ] A service identity cannot read another tenant's group, segment or inventory proof by guessing its identifier or replaying a cursor obtained under a revoked grant.
- [ ] A close-before-open event is recovered from full segment state and ordered history. A gap freezes only the affected resource-tenant projection/proofs until its contiguous sequence is restored.
- [ ] An expired history cursor returns explicit loss. Restarting at the retention floor does not clear the source-loss exception; complete snapshot/replay or authorized disposition supplies auditable resolution.
- [ ] A deleted resource remains in the historical scope for a served interval. An empty current resource list produces no proof of an empty historical hour.
- [ ] A bounded batch covers all requested windows, including pending outcomes and continuations. A month with 744 hourly windows is processed without one remote Subscriptions call per usage record; measured limits remain subject to S-Q07.
- [ ] An inventory barrier plus complete attribution can seal an empty ownership scope. It cannot substitute for the emitter's coverage declaration for a nonempty scope, nor override a known source loss.
- [ ] A late resource event resolves against ownership at its effective instant. An ambiguous match is quarantined, not assigned to today's active subscription.
- [ ] With no confirmed customer cancellation, a maintenance stop within the accepted SLA creates no charging cutoff merely from its cause. If a platform-fault or maintenance stop exceeds the accepted SLA, the adapter retains that qualifying evidence and the authoritative effective stop; ordinary accrual ends at that stop and Billing corrects any charged tail. Callback delay cannot shift the boundary, and operator/abuse/policy stops retain their suspension bill/waive treatment.
- [ ] A higher attribution version is recoverable together with the old version and causes explicit affected-result correction; a posted invoice and its payer remain unchanged.
- [ ] A held accepted price remains frozen after a new price is published. Expired hold replay remains historical but does not authorize activation; S-Q01 direction is accepted, but runtime activation remains gated until its versioned authorization/time contracts and fixtures are accepted.
- [ ] One-time routes with missing S-Q04 schema or S-U08/S-U13 evidence refuse before fee-bearing capture/activation despite accepted S-Q03/S-Q04 directions; existing advances must be applied by Billing without duplicate collection. Unsupported trial terms, overlays, collection pause or intra-hour tier transfer also fail before sales are enabled; missing providers are explicit dependency failures.
- [ ] A workload report covers the full proposed PRD §7 tenant/population/rate profile, skewed keys, 744-window proof batching and a cold attribution rebuild; every page retains scope/snapshot completeness and no per-usage-record lookup is introduced.
- [ ] Restart and failover with durable storage intact lose no acknowledged commercial commit, receipt or create-settlement fence. Recovered delivery reuses original business identities and cannot create a second effect.
- [ ] A disaster restore drill records actual RPO/RTO against the proposed ≤ 15-minute / ≤ 60-minute objectives, restores the declared recovery inventory and reconciles source evidence to facts before dependent completeness is asserted; unresolved history stays visibly pending.
- [ ] The ratified 99.9% monthly availability claim has an S-U11-approved SLI and a report over its stated observation window. The load or restore fixture alone is not accepted as availability evidence.
- [ ] Under fact v4, a mutable subscription read reports another quantity. Rating preserves v4's exact source inputs; a legitimate commercial change requires v5.
- [ ] An authorized engine-only re-evaluation preserves accepted terms. An operator request cannot substitute today's price without a separately authorized commercial correction.
- [ ] A worker evaluates v4 while v5 becomes authoritative. The stale result cannot replace v5's head; recovery retains the old evidence without claiming current completion.
- [ ] With v3 current, conflicting content for retained v1 is still rejected. Age does not legitimize a digest conflict.
- [ ] Changing a lot, stored scale, line amount, accepted reference or result flag changes the result digest. Transport retry/trace metadata alone does not. Producer codec vectors verify both outcomes.
- [ ] A partial-hour peak has no supported boundary split, while continuation and usage coverage are missing. Full valuation stays pending with all blockers bound to the exact fact. Restart and resolution of one blocker do not hide the others.
- [ ] Stored scale and accepted numeric limits survive historical replay. Overflow and unsupported precision refuse without rounding, clamping or zero substitution.
- [ ] Provider capability disappears after purchase. The original obligation remains visible for recovery/remedy; there is no lot flattening, duplicate capture or silent zero valuation.
