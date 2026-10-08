---
description: "Versioned commercial facts, composition seals and recoverable publication for Rating and Billing consumers."
---

# Feature: Commercial Facts and Composition Seals

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Publish the committed commercial outcome](#21-publish-the-committed-commercial-outcome)
  - [2.2 Recover a missed commercial-period opening](#22-recover-a-missed-commercial-period-opening)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Publish sufficient lifecycle and financial context](#31-publish-sufficient-lifecycle-and-financial-context)
  - [3.2 Preserve business ordering and replay](#32-preserve-business-ordering-and-replay)
  - [3.3 Derive stable facts and complete accepted term slices](#33-derive-stable-facts-and-complete-accepted-term-slices)
  - [3.4 Preserve payer and order lineage](#34-preserve-payer-and-order-lineage)
  - [3.5 Keep commercial state separate from invoice state](#35-keep-commercial-state-separate-from-invoice-state)
  - [3.6 Seal the complete commercial set](#36-seal-the-complete-commercial-set)
- [4. States (CDSL)](#4-states-cdsl)
  - [4.1 Publication readiness](#41-publication-readiness)
- [5. Definitions of Done](#5-definitions-of-done)
  - [5.1 Atomic and recoverable publication](#51-atomic-and-recoverable-publication)
  - [5.2 Complete and stable financial inputs](#52-complete-and-stable-financial-inputs)
- [6. Acceptance Criteria](#6-acceptance-criteria)

<!-- /toc -->

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-featstatus-commercial-facts-implemented`
- [ ] `p1` - `cpt-cf-bss-subscriptions-feature-commercial-facts`
## 1. Feature Context

### 1.1 Overview

Publish immutable versions of commercial obligations without monetary amounts, plus the complete fact set for each payer-specific billing group. Publish usage parent facts when commercial periods open. Rating schedules and values their evaluation windows.

This is the **target producer design**, not evidence of delivered code or consumer agreement. S-Q03 assigns valuation to Rating; S-Q04 assigns durable identity per agreed chargeable action. [DECISIONS.md](../DECISIONS.md) and [UPSTREAM_REQS.md](../UPSTREAM_REQS.md) retain occurrence schema, S-U08/S-U13 routing/accounting/runtime and S-Q05 confirmation protocol and owner-contract gates. Approving the direction alone does not enable fee-bearing capture or activation.

### 1.2 Purpose

**Requirements**: `cpt-cf-bss-subscriptions-fr-event-producers`, `cpt-cf-bss-subscriptions-fr-event-payload-completeness`, `cpt-cf-bss-subscriptions-fr-event-ordering`, `cpt-cf-bss-subscriptions-fr-recurring-idempotency`, `cpt-cf-bss-subscriptions-fr-no-retro-edit`, `cpt-cf-bss-subscriptions-fr-billing-traceability`, `cpt-cf-bss-subscriptions-fr-dataset-separation`, `cpt-cf-bss-subscriptions-nfr-recurring-cut`.

**Additional requirements**: `cpt-cf-bss-subscriptions-fr-versioned-billable-facts`, `cpt-cf-bss-subscriptions-fr-usage-parent-facts`, `cpt-cf-bss-subscriptions-fr-billing-groups-seals`, `cpt-cf-bss-subscriptions-fr-one-time-occurrence`.

**Supporting requirements (primary owner elsewhere):** `cpt-cf-bss-subscriptions-fr-accepted-price-bindings`.

**Primary NFRs**: `cpt-cf-bss-subscriptions-nfr-recurring-cut`, `cpt-cf-bss-subscriptions-nfr-proration-accuracy`.

**Principles**: `cpt-cf-bss-subscriptions-principle-when-not-math`, `cpt-cf-bss-subscriptions-principle-effective-dated-composition`.

### 1.3 Actors

| Actor | Role in Feature |
|---|---|
| `cpt-cf-bss-subscriptions-actor-rating` | Consumes commercial facts; owns all amount, proration, minimum-fee and hourly-window calculations |
| `cpt-cf-bss-subscriptions-actor-billing` | Consumes commercial composition and Rating results; owns financial posting and corrections |
| `cpt-cf-bss-subscriptions-actor-platform-operator` | Investigates failed period reconciliation or publication |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md), events and financial alignment requirements.
- **Design**: [DESIGN.md](../DESIGN.md), commercial facts, immutable bindings, billing groups, outbox and recovery interfaces.
- **Decomposition**: [DECOMPOSITION.md](../DECOMPOSITION.md).
- **Build prerequisites**: [Foundation](01-foundation.md) and [composition](02-composition.md), matching DECOMPOSITION; fact storage and producer contracts can be built against their accepted interfaces.
- **Integration acceptance**: [renewal/retention](04-renewal-retention.md) supplies accepted period geometry, [tenancy/import](07-tenancy-import.md) supplies historical payer allocations, and [consumer contracts](09-consumer-contracts.md) exercises recovery/attribution against the stored facts. These are later conformance requirements, not reverse build dependencies from storage to its consumers. S-Q03 selects Rating one-time tariff valuation; end-to-end routing/accounting still requires S-U08/S-U13, with S-Q04 exact schema and S-Q05 confirmation protocol and owner-contract gates.
- **Consumer contracts:** [Rating contract](../DESIGN.md#rating-result-contract) and [financial consumer boundary](../DESIGN.md#financial-consumer-boundary) define the fact, valuation and recovery boundaries.

## 2. Actor Flows (CDSL)

**Use cases**: internal lifecycle-to-finance flows; the PRD's composition-change and renewal scenarios consume the same processes.

### 2.1 Publish the committed commercial outcome

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-flow-emit-evt`

**Actor**: lifecycle transition or commercial-period worker, observed by Rating and Billing.

**Success Scenarios**: activation opens the first partial period; renewal opens the next period; quantity, suspension, cancellation or composition changes replace the affected fact versions. An exact retry returns the original outcome.

**Error Scenarios**: stale aggregate revision; incomplete accepted terms; unsupported policy; a fact identity/version with conflicting content; an unresolved capability gate. No successful activation outcome is published for a pending operation.

**Steps**:
1. [ ] - `p1` - Stage the accepted binding, external eligibility and policy observations outside the database transaction. Apply the lifecycle feature's freshness checks; an old hold replay is not fresh permission to activate. - `inst-subscriptions-publish-stage-evidence`
2. [ ] - `p1` - Derive the affected commercial periods and payer allocations from committed effective-dated history. Compare the expected aggregate and composition revisions before committing. - `inst-subscriptions-publish-derive-periods`
3. [ ] - `p1` - Apply the fact identity and term-slice processes in §3.3. Allocate a new version only when commercial content changes; preserve the same economic identity on retries and corrections. - `inst-subscriptions-publish-version-facts`
4. [ ] - `p1` - Commit the lifecycle outcome, operation receipt, accepted commercial records, fact versions, any completed composition seal and outbox rows atomically. A crash leaves either the complete commit or none of it. - `inst-subscriptions-publish-atomic-commit`
5. [ ] - `p1` - Deliver the stored events at least once. Consumers recover or deduplicate by immutable business identity, independently of broker delivery identifiers. - `inst-subscriptions-publish-deliver-events`

### 2.2 Recover a missed commercial-period opening

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-flow-reconcile-commercial-periods`

**Actor**: period reconciler; `cpt-cf-bss-subscriptions-actor-platform-operator` handles unresolved discrepancies.

**Success Scenarios**: an outage spanning several period boundaries creates the missing parent facts and seals exactly once, including a period that began before cancellation.

**Error Scenarios**: missing historical binding or period history; failed dependency; an unsupported backdated change. Persist an exception and leave the affected obligation incomplete; do not reconstruct accepted prices from today's catalog.

**Steps**:
1. [ ] - `p1` - Run a bounded recurring reconciliation schedule over periods due from the durable commercial-period cursor and effective-dated lifecycle history. Under the ratified PRD §7 workload and healthy required providers, the scan interval plus queueing and repair must fit the ratified 15-minute detection-and-repair budget measured from the missed publication boundary. Record the actual schedule and measurements as S-Q07 conformance evidence; target approval alone proves no measured result. Live subscription status alone is insufficient. - `inst-subscriptions-repair-scan-periods`
2. [ ] - `p1` - Include every component interval overlapping a served period and exclude periods starting at or after its authoritative service end. For an unresolved cancellation-tail policy, block that capability rather than infer an end from purge time. - `inst-subscriptions-repair-enumerate-intervals`
3. [ ] - `p1` - Upsert through the same identity and version process as §2.1. Reconcile the expected commercial set before sealing it. - `inst-subscriptions-repair-upsert-facts`
4. [ ] - `p1` - Advance the period cursor in the transaction recording the completed work. A retry never allocates a second fact for the same obligation. - `inst-subscriptions-repair-advance-cursor`
5. [ ] - `p1` - Expose recovered facts through `period_facts`, `groups_due` and `facts` even if no event reaches a consumer. A daily audit is a secondary completeness check; it is not the detector relied on for the bounded repair budget. Neither schedule supplies Rating's hourly clock. Missing provider/evidence prevents unsafe repair, leaves a visible exception and backlog age, and must not be reported as successful recovery. - `inst-subscriptions-repair-expose-recovery`

## 3. Processes / Business Logic (CDSL)

### 3.1 Publish sufficient lifecycle and financial context

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-producers-evt`

Event publication is paired with the [Subscriptions recovery journal](09-consumer-contracts.md#38-recover-gaps-cursor-loss-and-retained-history). The local transaction appends immutable recovery references for each committed fact replacement and affected group/seal version alongside its outbox work. Snapshot discovery plus commit-position catch-up must expose a correction to an old period even if its due-date range was already scanned and every event is lost. A seal remains an immutable reference to its declared complete commercial member versions; journal ingestion or snapshot exhaustion alone is not Rating completeness or Billing financial finality.

The canonical event vocabulary and required fields live in DESIGN. This process assembles them only from the committed version:

- Lifecycle creation, activation, suspension, resumption, cancellation and composition changes identify their aggregate version, effective instant, immutable tenant axes, initiating operation and correlation. Order-originated changes preserve order, order-version, attempt and order-line references where applicable.
- `BillableFactPublished` publishes an immutable commercial fact version. `BillableSetSealed` names the complete commercial fact set, including one-time obligations when that capability is enabled. Neither is a priced invoice line.
- `SubscriptionTransitionOutcome` distinguishes accepted work from successful or terminally failed activation, void or compensation. A compensation disposition proves durable financial obligations, not a posted credit note.
- `EntitlementPostureChanged` describes Subscriptions' commercial decision. Entitlements owns materialized license and pack events.
- Attribution and scope events use the separate resource-tenant stream and proof rules in feature 09. Subscription ordering does not imply resource-tenant ordering.
- Scheduling/withdrawal, renewal/grace, quantity change and trial outcomes are emitted by their owning lifecycle features only when the corresponding capability is enabled. A registry entry does not authorize an unsupported trial or collection policy.

### 3.2 Preserve business ordering and replay

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-ordering-evt`

Lifecycle sequence assignment and outbox persistence share the state transaction. The `(orderingTenantId, subscriptionId)` key remains pinned throughout the lifecycle; payer transfer is deferred. Each event also names its business entity and version. A consumer can then recover from a duplicate, gap or older version without treating arrival order as commercial truth.

The same entity/version/digest is an exact replay. The same entity/version with a different digest is a conflict that requires investigation. Lower versions remain readable but never replace the head. The attribution stream has its own resource-tenant sequence; consumers may not compare the two sequence spaces.

### 3.3 Derive stable facts and complete accepted term slices

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-recurring-evt`

**Input**: a committed commercial period, effective component and quantity intervals, accepted bindings, billing terms, payer allocations and the applicable suspension billing posture.

**Output**: one stable parent identity per commercial obligation and an immutable replacement version when its content changes.

1. Preserve the component interval identity. Existing `lineKey` values `plan#n` and `addon:{addOnId}#n` remain stable aliases: re-adding a component allocates a new lifetime interval ordinal; a quantity update changes its timeline. This component identity is not a one-time charge-occurrence identity: a new interval or item does not itself authorize a fee. Price replacement alone never creates another charge occurrence.
2. Distinguish subscription, component occurrence, PriceBook item, charge kind, payer allocation and commercial period. An item may carry a different charge kind or allocation. Assign one stable target fact identity per obligation and retain its purchase/component lineage; corrections and replay must not publish a second independent obligation.
3. Freeze period identity from the accepted billing anchor and cycle terms. Preserve half-open UTC geometry and the no-drift month-end clamp. An allowed cycle change starts a new sequence at the agreed boundary; it cannot re-key a previously issued fact.
4. Publish the `usage` parent at period opening, including activation's first partial period and lines with no observed usage. Its nominal served extent is versioned when service actually ends. Pin `UsageRatingPolicy`; an absent or unsupported policy blocks the affected sale. No Subscriptions hourly timer or hourly fact is created.
5. Partition the fact's served extent exactly once into ordered, non-overlapping temporal term slices. Cut at composition, any applicable accepted price `ends_on`, quantity and policy boundaries. For quantity-bearing components, every slice carries the complete effective purchase-lot allocation described below; each lot retains its own frozen binding and quantity. Several lots inside one slice do not create overlapping temporal slices. An implicit missing binding, unexplained time gap or duplicate temporal coverage refuses publication.

   For an established obligation with a known unresolved continuation, publish an explicit `Unresolved` interval instead of an implicit gap or invented binding; retain the known boundary evidence, reason and affected lot/quantity/purchase provenance (SUB-D-42). This fact describes the whole commercial state but cannot yet be fully priced.
6. Resolve a new binding where PriceBook requires it using the historical effective date and retained pin. Automatic adoption at `ends_on` inside the running term requires explicitly accepted continuation policy and satisfaction of its notice/assent prerequisites; Pricing closure/selection alone grants no such authority (SUB-D-41). Freeze the authorized `AcceptedBinding`, plan/revision/item/entry/price identities, SKU version, currency and invoice inputs. Read `ends_on`, never infer validity from stored `effective_to`; the accepted binding is not Rating's calculation snapshot.
7. Carry suspension ranges clipped to the period and the explicit bill/waive mapping and committed quantities. Rating computes any time/quantity proration and monetary consequences. Collection pause is deferred; facts carry no collection-pause posture in this version.
8. For a removed obligation, publish a higher version with explicit zero obligation and preserved identity, prior extent and correction lineage. An empty slice set is valid only for an explicitly zeroed obligation; it must not disguise missing terms.

For a late-discovered `ends_on`, obtain Pricing evidence outside the local mutation transaction, then recheck the expected subscription and fact heads against concurrent cancellation, quantity change and renewal. Commit an authorized complete continuation, affected successor facts, exact successor seal and outbox atomically. If continuation authority, required notice/assent or covered pricing is missing for an established obligation, atomically publish a full successor fact with the explicit unresolved interval, exact successor seal and outbox (SUB-D-42).

Rating advances the fact head and leaves full valuation pending. Do not publish the known prefix as a complete result or treat the unresolved tail as zero. A resolving continuation creates another fact/seal version. Billing cannot substitute an old fact result under the new seal, and an observed newer head blocks stale target acceptance; already posted effects are not automatically reversed. Preserve normal financial execution recovery. Provider unavailability alone does not prove that the boundary changed. This representation never bypasses admission/capture/activation/increase prerequisites.

This does not prescribe shutdown, free service or debt. Historical replay retains recorded terms. A freshness check confirming unchanged accepted content adds evidence, not a fact/seal revision. The concrete observation and recovery protocol remains proposed in the [Rating profile](09-consumer-contracts.md#32-consume-current-pricebook-providers-and-serve-frozen-bindings).

**Lot-aware replacement contract**

This refines the existing fact/version contract. Field roles below describe required evidence; exact SDK names and transport encoding still require owner agreement.

| Level | Required evidence | Invariant |
| --- | --- | --- |
| Fact | Stable fact ID, version, digest, component/item, charge kind, billing period, immutable payer/group context and source commercial revision | A same-obligation quantity increase replaces the existing fact; it does not append an independent charge for the resulting total |
| Temporal slice | Half-open start/end, applicable posture/policy and complete lot allocations | Slices partition served extent; there is one temporal slice at any served instant |
| Lot allocation | Stable purchase-lot ID, effective quantity, immutable accepted-term reference and purchase-business provenance | Each effective lot appears once in the slice; compatible quantities sum to committed component quantity; all historical terms are recoverable |
| Commercial seal | Successor composition version and exact `(fact_id, fact_version, payload_digest)` references | Fact membership and exact versions remain coherent even when only an existing fact changes |

For an illustrative recurring per-unit component over `[periodStart, periodEnd)`, original purchase A contains ten units under binding A. A new purchase B adds two units under binding B at actual applied T. Version 1 contained A=10 for the period. Version 2 retains the same fact ID and contains the full corrected timeline:

| Temporal slice in version 2 | Complete lot allocation | Service quantity |
| --- | --- | --- |
| `[periodStart, T)` | A: 10, original binding and purchase origin | 10 |
| `[T, periodEnd)` | A: 10, original binding; B: 2, delta binding and purchase origin | 12 |

The second row is one slice with two allocations, not two overlapping slices. Rating computes the accepted tariff and applicable partial-period treatment; Subscriptions sends no calculated EUR amount. The paid-group/charge-kind/period identity is unchanged in this example. A new component or a separately required payer/period boundary follows the existing obligation identity rules instead of forcing unrelated charges into this fact.

At submission, authorize and resolve the original purchase business identity before creating a lot. A repeated operation, including with another transport key, recovers the same lot and durable outcome; changed business content conflicts. A new Orders version/attempt is not an implicit second purchase or replay and remains subject to existing S-U01 settlement/association rules. Validate the expected aggregate revision, resulting quantity and all accepted evidence.

At application, commit the lot, composition/quantity history, operation result, full affected fact versions, successor seal and publication outbox in the same local transaction. No consumer-side call occurs inside it. A failed commit exposes no partial purchase. A lost response or publication is recovered from the original result/outbox without another lot, revision or charge.

Rating consumes fact versions as full replacements. The same ID/version/digest is a no-op. The same ID/version with different content conflicts and preserves the previously accepted state. An older delivered version cannot overwrite the newer accepted head. A newer version with missing required evidence remains visibly pending until recovery. A broker acknowledgement alone proves neither valuation nor invoice completion. Corrections retain lot identity and historical accepted terms, with an explicit predecessor chain.

Billing uses the complete canonical rated replacement and successor seal under its normal draft replacement or posted correction path, never sums version 1 and version 2 as independent obligations. Authorized historical corrections remain distinct from deferred customer-requested quantity reductions.

Single-lot allocations use this same model. New obligations use this model directly; updates retain their original purchase lineage and fact ID. The lot representation does not itself allocate pooled metered usage or change the tariff scope; the accepted flat/minimum rules below govern the supported valuation profile. No resource-reassignment API or deferred decrease mechanism is required for this increase profile.

**Flat and period minimum.** Rating charges recurring flat once per component obligation for covered time under identical applicable flat terms, not once per purchase lot or seat (SUB-D-46). Purchase provenance remains distinct. Different simultaneous flat tariffs on one component are unsupported; sequential authorized continuation uses separate slices. Recurring supports flat/per-unit, not usage-only tier/package models. Recurring proration and proportional period-minimum calculations use exact covered UTC seconds divided by the billing period's UTC seconds.

The minimum applies once per price/subscription/period/currency/scale to eligible base charges, with coverage as the union of chargeable intervals. Equal-valued but different price IDs have separate floors. Subscriptions retains terms, quantities and interval evidence; Rating derives the money. Cross-fact aggregation requires its agreed contract; an independent per-fact floor is not a substitute.

The resource-scoped usage-minimum profile requires a separately enabled Pricing/Rating contract. Its accepted terms select eligible consumption, such as storage or traffic, and either an individual resource or a group with historical membership. Individual and group minima are alternatives; the same usage does not receive both. The monetary minimum applies after valuation of eligible usage. Accepted window options are calendar hour and billing period.

A partial eligible calendar hour carries the full hourly minimum once per selected target/window, without time proration or multiplication by group members. Thus 10:50–11:10 covers two hourly minima; excess in one hour does not offset the other. Partial billing-period treatment is an explicit accepted setting: full minimum or proportional to eligible time. Cross-price eligibility, exact resource/group identities, timezone/SDK mapping and eligible-duration evidence remain required integration contracts (SUB-D-47 through SUB-D-54). See the [valuation contract](08-commercial-facts.md#33-derive-stable-facts-and-complete-accepted-term-slices).

**Quantity-allocation capability gate:** a same-item increase whose accepted delta binding differs from the existing quantity's binding must refuse before commercial application until the accepted semantic contract above has an agreed SDK/consumer version and runtime evidence under S-U08. Temporal slicing alone cannot represent concurrent old-price and new-price quantities. The contract must preserve immutable quantity-lot identity, each lot's accepted binding/provenance and effective extent, require the complete allocation to sum to the committed service quantity at every served instant, and retain the existing quantity's terms.

Replay and correction need an unambiguous accepted mapping to those lots without duplicating or repricing an obligation. Initial-profile decreases/removals and transfers between commercial lots refuse before effects; wire encoding and owner integration remain open. This gates delivery of the separately priced delta behavior, not the behavior itself. It does not block same-binding quantity increases or separately identified logical add-on items under their existing guards.

**One-time direction and gate**: S-Q04 assigns a durable charge-occurrence identity to each separately agreed chargeable business action. Retry after failure, another transport key/attempt or a rebuilt draft keeps the same action and occurrence; it is not a new purchase. Resume of the suspended subscription and catalog price/version changes create no new setup fee. A new connection purchase after cancellation may create a distinct fee only when its new agreed purchase terms explicitly include it.

Cancellation/re-add, plan change, trial conversion and technical reprovision alone are not charge triggers; distinct agreed purchases remain distinct even at the same price. S-U01 successor/rebind gates and S-Q02 protected order rights remain unchanged.

The exact wire identifier/schema, including any proposed `occurrence_id` field, is not approved by this product direction. Identity is independent of `price_id` and price version. Allocating an identity, reserving or prepaying does not prove service or earn the fee: the committed qualifying action and accepted terms drive its amount-less fact. Corrections retain the same occurrence with explicit versions. Import and legacy occurrence migration are excluded from this release; no imported-setup fixture or mapping is required.

**One-time calculation basis.** The fact retains the qualifying action, its applied evidence and frozen accepted terms, with `basis = Once` for `flat` or `Units { quantity, unit }` for `per_unit` (SUB-D-43). Units contains the positive exact quantity agreed for that action and its unit; it never reads current subscription quantity or defaults to one. Once charges the agreed flat action once, without multiplying by seats. Rating rejects missing/invalid quantity or incompatible model/unit. Later subscription growth does not change the earlier occurrence/quantity.

A separately agreed additional chargeable action creates a distinct occurrence; correction of the original action uses a full successor fact and exact seal under its existing occurrence. This accepts the semantic variants, not an implemented wire codec or independent provider approval.

S-Q03 routes the fact to Rating for frozen-term tariff valuation, then Billing for accounting/posting, existing-advance application and final invoice rounding/corrections. Taxes/payments retain their owners. Occurrence schema, routing, binding/calculation-snapshot mapping and replay/correction/prepayment fixtures remain S-U08/S-U13 gates. Fee-bearing capture/activation stays disabled until those contracts and runtime evidence are demonstrated; neither S-Q04 nor allocating an identity waives pre-capture protection or enables a previously unsupported one-time change/trial profile.

### 3.4 Preserve payer and order lineage

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-traceability-evt`

Every version retains its component's own SKU, accepted plan/revision/item/price binding, subscription and occurrence/period identity. Order references, including an external purchase-order reference where present, follow the sold component into facts and recoverable history.

A billing group's payer, seller, currency and billing window are immutable. Delayed processing reads historical payer allocation, never the subscription's current payer. Payer and resource-owner transfer is deferred; a billing group's payer never changes in place. Supported hourly tier, scope, meter or policy changes take effect at the next UTC-hour boundary; unsupported intra-hour requests fail before applying commercial state. More permissive PerUnit splitting requires authoritative source interval support and joint acceptance.

### 3.5 Keep commercial state separate from invoice state

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-dataset-priceoverride-evt`

See [RO-2026-10-15](04-renewal-retention.md#7-renewal-and-offboarding-integration-contract).

Subscriptions never infers posting from publication, rewrites a posted line or calculates a monetary reversal. Cancellation, order compensation and correction publish replacement obligations; Rating computes full replacement results; Billing compares the accepted group target against financial history. A published zero version does not mean a refund has posted.

For the accepted PlanRevision credit-trial variant, publish legitimate served/usage facts with unchanged ordinary price bindings before paid-target conversion; Rating values the gross tariff normally. Trial-credit policy/grant references may accompany money-free facts under the agreed contract, but bonus amounts, balance and allocation math do not become Subscriptions fields. Billing owns the shared benefit across eligible items and consumes canonical full Rating replacements, retaining prior accepted allocation and result/version lineage. It adjusts the same grant once under original terms/scope/currency; duplicate versions are no-ops and conflicting version/content refuses/recovers.

This introduces no independent delta delivery. Valid/applicable-grant correction may restore availability for an active trial or eligible paid usage after early guarded conversion, only until the same original expiry. An early valid guarded paid transition retains unused credit on the same grant for eligible paid-plan expenses until its original expiration, with unchanged currency/product/item/charge-kind restrictions. Closing the trial by this conversion does not revoke that eligibility or extend the grant; balance alone grants no service permission.

Rating still values paid usage from its frozen accepted tariffs; Billing allocates the retained grant, keeping gross valuation and customer payable separate. After grant expiry, history/accounting still corrects but spendable credit is zero and expiration is unchanged; no trial extension follows. A later correction cannot automatically reopen a trial already ended by exhaustion, even before original expiry: restored accounting balance grants no service/spend permission for that closed episode and does not reverse an applied paid conversion.

V1 has no closed-episode reactivation, rollover past original expiry or same-subscription regrant. A separate new acquisition is a fresh paid purchase subject to its supported association/recovery contract; repeat trials are deferred. Billing uses standard draft replacement/posted correction and the agreed Ledger path, without inline rewriting of posted invoices. S-U15 exact provider/integration remains OPEN; the cash-funded Ledger wallet API is not a free-bonus grant. Gross rated value, bonus coverage, platform-borne amount, customer obligation and access/paid-target boundaries are distinct.

No-paid-start/expiry-no-debt rules must not erase trial evidence or imply a zero tariff. Currency/charge-kind applicability and S-Q04 qualifying occurrence still govern; a grant is neither capture nor a setup-charge trigger. For eligible accepted-trial expenses served before the authoritative applied paid boundary, the platform bears full rated value beyond bounded bonus coverage, including positive corrections, errors and accounting/stopping-delay or in-flight overrun; it creates no customer trial-overage debt. Usage actually served after a valid guarded paid transition follows accepted paid-plan/settlement terms after eligible bonus coverage.

Classify by served time and the applied boundary, never rating/event/correction arrival; split crossing usage using supported evidence and never backdate paid conversion. Rating preserves actual gross usage/value; Billing keeps the bounded grant allocation separate from the platform-borne amount, without negative bonus, unlimited top-up or treating the whole gross value as covered. Erroneously posted trial tail uses existing idempotent Billing correction and agreed Ledger paths. Subscriptions retains evidence/lineage rather than raw metering or allocation math.

For the illustrative full replacement 90→110 against grant 100, eligible pre-boundary trial service yields bonus 100/platform 10/customer 0 once, preserving gross and original source/tariff/version lineage; duplicate versions cause no additional grant or adjustment. S-U15 exact evidence/provider/enforcement and liability/correction contracts outside that accepted scope gate launch; this waives no prior debt, explicit retention charge or excluded charge kind and selects no refund method/accounting schema.

For one-time charges the selected division is tariff valuation in Rating and financial accounting in Billing. Rating replays the same qualifying fact/version against its frozen agreed terms; correction retains original accepted terms and version lineage rather than reading a fresh catalog. Billing applies any existing advance and deduplicates invoice/posting effects; valuation or activation cannot itself request collection of an already prepaid setup charge. This selects no refund policy or repeat-charge trigger.

An order reservation and durable future commercial commitment create no supplied-service fact, access or normal service period. Billing/Payments may capture prepayment while the subscription remains draft, only after the Foundation's required reservation, commercial-commitment and capture contracts are satisfied. That money has its own financial evidence; it cannot be represented by inventing served subscription coverage. Cancelling a paid unactivated order therefore records the agreed financial-remedy obligation even when no service fact needs correction.

Subscriptions does not infer capture failure from timeout, compute refunds or claim settlement from a reservation release; unknown financial outcomes remain recoverable under the owner protocol.

S-Q05 accepts both the post-stop rule and the explicit customer-confirmed cancellation charging cutoff. Platform technical delay after that cutoff creates no ordinary customer tariff even if actual stop is late or unknown. Authoritative resource provider/usage owners keep truthful runtime/usage at its original coordinates. Commercial facts retain relevant evidence/lineage and the nonchargeable technical extent with frozen policy and confirmation evidence under the agreed contract; Subscriptions acquires no raw-usage ingestion or sample-storage responsibility. Do not erase usage, set catalog prices to zero or substitute collection-pause posture.

A future scheduled cutoff leaves normal pre-cutoff service under existing terms; request receipt alone supplies no cutoff. Authoritative cancellation/offboarding stop also ends ordinary accrual, regardless of a later scheduled date or callback. This is not a change to suspension bill/waive policy.

Versioned replacement facts/results carry the financial correction without fabricating earlier physical stop or commercial-state application. Rating owns tariff math; Billing corrects an erroneous tail charge/posting, applies allocated advances and settles overpayment idempotently through its agreed financial-remedy path. This is correction of an incorrect charge, not extra goodwill compensation; cash-refund method/timing and credit formula remain unselected. Late pre-cutoff usage/corrections retain original attribution, frozen terms and version lineage. Legitimate prior debt/commitments remain; erroneous technical-tail charges cannot be preserved as prior debt.

Resource presence/export/hold/delayed deletion alone creates no ordinary service or storage charge; explicit agreed retention terms, including existing terms, may support their own obligation. Unknown stop remains visible operational/closeout uncertainty but cannot erase a known cutoff or justify indefinite billing. S-U05/S-U13 must accept the versioned confirmation/stop/correction and capture/compensation/state contracts. Customer confirmation follows authorization/eligibility and durable cutoff evidence, independent of resource provider completion. Withdrawal/rescheduling needs explicit versioned confirmation before the applicable cutoff and cannot restore charges retroactively; post-cutoff new service requires a separate valid guarded route.

Exact protocols remain open; internal stops are split by cause. Financial cutoff alone does not release occupancy or permit deletion/archive; paid-order remedies remain unchanged.

Contract amendments may select future effective terms only through an accepted, versioned input contract. Unsupported overrides remain gated; a binding reference cannot authorize Subscriptions to evaluate an overlay itself.

### 3.6 Seal the complete commercial set

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-algo-seal-commercial-set`

See the [financial consumer boundary](../DESIGN.md#financial-consumer-boundary).

1. Enumerate all obligations for the payer-specific billing window from effective commercial history, including every line, PriceBook item and enabled charge kind. The enumeration is independent of received usage or completed Rating results.
2. Ensure every enumerated obligation has a stored fact version. Include explicit zero versions for removed obligations so earlier financial effects remain reversible.
3. Store an immutable composition version, previous-version reference, sorted exact `(fact_id, fact_version, payload_digest)` references and content digest. Include each member once; the set digest binds the exact references. Atomically publish the seal with the final facts it references. An interrupted assembly remains unsealed and cannot imply completeness.
4. A later composition change publishes new fact versions and a successor seal; it never edits an old seal. A fact content correction with unchanged membership still advances the required version in a successor seal.
5. Keep the complete set recoverable by group/version. A seal is proof of commercial membership only. It neither proves usage coverage nor substitutes for Rating's complete child-window manifest or Billing's invoice checks.

**Exact references.** A seal's member versions are exact, never minima (SUB-D-40). For each member, Billing verifies equality of the source fact ID, version and digest carried by the selected complete Rating result. A result for a newer fact version is staged until a successor seal names that exact reference; an older version or conflicting digest cannot satisfy the member.

A later Rating result revision for the same exact fact may replace the prior result under the same seal, including a corrected usage manifest, provided the new result is complete and all result/financial predecessor guards hold. It does not create a new commercial composition. An already observed newer commercial head still prevents accepting a stale financial target: recover its seal instead of falling back to the old cut.

Example: seal 7 names A/v4/d4. A result based on A/v5/d5 waits for seal 8 naming A/v5/d5. A new calculation result r12 based on A/v4/d4 can supersede r11 under seal 7 without another commercial seal, provided no newer commercial head has already made that target stale. Billing records the exact selected result/manifest references in its own new target and retains invoice-version and uncertain-execution recovery.

**Proposed financial compatibility and recovery contract: S-U08/S-U13**

This matrix fixes the minimum semantic evidence for a reviewable producer/consumer draft. Exact schema, owner ratification and runtime conformance remain open. Names already used above retain their target meaning; no row invents a shipped endpoint. Subscriptions publishes commercial facts/references, not rated amounts, invoice adjustments or bonus allocation values.

| Input / requested evidence | Minimum identity and version boundary | Outcome, recovery and fixture |
| --- | --- | --- |
| Accepted binding → Rating calculation input | Fact/version and term-slice extent; complete accepted binding/policy reference and digest, SKU/plan/revision/item/price identities, quantity/posture and payer/currency provenance. Rating separately records its calculation snapshot/result identity and version | A mapping receipt preserves both authorities and exact source digest; missing/unsupported historical input stays pending. Never replace a missing snapshot by current catalog money or equate an accepted-binding ID with Rating's calculation ID. V05/V08 replay after catalog change uses original inputs |
| Recurring/usage obligation and replacement | Stable fact ID; immutable BillingGroup coordinates; fact version/digest, period/served extent, frozen slices and correction predecessor. An explicit zero-obligation replacement retains the old identity/extent/lineage | Same version/digest replays; conflict refuses, lower version cannot overwrite the head. Missing slices are not zero obligation. V07/V10 recover a lost opening, later shortened extent and zero replacement without duplicate economic identity |
| Separately agreed one-time action | Durable purchased-action/occurrence evidence, original accepted terms and committed qualifying action; origin/line/component association where applicable; fact/result version lineage independent of price and transport attempt | A retry/rebuild/reprice/resume cannot create another fee. Distinct agreed purchases remain distinct even at the same price. S-U01 successor gates still apply; unsupported charge paths refuse. V08 covers original action replay and an explicitly agreed new purchase after cancellation |
| Commercial set seal | BillingGroup, composition version/previous version, exact `(fact_id, fact_version, payload_digest)` member references, set digest and seal time | Consumer verifies complete membership and exact source-reference equality at one recoverable read barrier. Missing member/version is pending, not removed; a result for a newer fact waits for its successor seal. Newer corrected content cannot be overwritten by a prior seal; removal remains explicit zero. V10 loses every event and recovers the complete set |
| Canonical full Rating results and completeness | Source fact/version, calculation snapshot, rated result/window identity/version/digest and complete expected child-window manifest, with predecessor result lineage | Billing accepts full replacements with version guards; no independent delta-only input. A commercial seal does not prove usage/Rating completeness. Missing child/window/source proof blocks finalization; a proven empty scope is distinct. V07/V10 exercise delayed child, correction and reordered delivery |
| Billing target / draft / posted correction | Accepted group/set and complete Rating-result versions, prior financial target/invoice revision, immutable posting/correction references and stable operation identity | Billing compares the new full target with prior accepted financial history: supported draft replacement or a versioned posted correction, never inline posted rewrite. Lost reply recovers the same operation. No publication/zero fact is proof of refund or posting. V08/V10/V13 assert no duplicate posting/credit |
| Existing advance allocation | Authoritative payment/advance identity, scope/currency and qualifying purchase obligation, prior Billing allocation version and remaining applicable advance evidence | Billing applies the existing allocated advance once; the tariff remains its full rated value. A partial advance reduces remaining due only. Retry/correction retains the allocation lineage; unknown finance is not zero or a second capture. V08 uses a separately specified expected full/partial advance case |

Recovery records which source head, set/result manifest and financial target each accepted outcome used. A contradictory digest or missing retained range returns an explicit conflict/pending outcome without synthesizing invoice completeness. Tests must compare original accepted terms, independently specified Rating outputs and Billing financial outcomes; they may not use Subscriptions monetary code to generate expected answers. Supported correction preserves prior financial lineage even when operational resources or catalog entries no longer exist.

## 4. States (CDSL)

### 4.1 Publication readiness

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-state-commercial-publication`

**States**: `incomplete`, `sealed` for a composition revision. These are financial-publication states, not subscription statuses.

**Transitions**: a revision stays incomplete while its commercial description has an implicit gap or an expected fact is missing. For an established obligation, a known unresolved continuation is a complete description only when it carries the explicit `Unresolved` interval, reason and boundary/lot evidence required by §3.3 and SUB-D-42; missing continuation authority or pricing does not block that successor seal. This does not relax new-admission guards or establish Rating completeness. It becomes sealed only by §3.6's atomic commit.

A sealed revision is immutable; later changes create a new revision. Delivery retries do not change readiness. The fact-version chain is insert-only with a monotonically advancing head, including zero-obligation replacements.

## 5. Definitions of Done

### 5.1 Atomic and recoverable publication

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-dod-commercial-publication`

The system **MUST** persist state, receipt, facts, completed seals and outbox consistently, recover missed period work from history, and expose the same immutable versions through recovery reads.

**NFR acceptance evidence:** measure period-opening/change-to-publication lag against the ratified p95 < 5 s / p99 < 30 s targets using [PRD §7's reference workload](../PRD.md#7-non-functional-requirements). Deliberately skip an opening and a seal publication under otherwise healthy required providers, and measure discovery plus durable repair within the ratified 15-minute budget without waiting for the secondary daily audit. Repeat across restart and concurrent producers, retaining original identities. Record blocked-provider cases separately as pending exceptions. Targets are ratified under S-Q07 and remain undemonstrated until measured.

**Implements**: `cpt-cf-bss-subscriptions-flow-emit-evt`, `cpt-cf-bss-subscriptions-flow-reconcile-commercial-periods`, `cpt-cf-bss-subscriptions-algo-ordering-evt`, `cpt-cf-bss-subscriptions-algo-producers-evt`.

**Touches**: lifecycle transactions, commercial-period cursor, fact/version and seal stores, event outbox, `SubscriptionBillingReadV1`.

### 5.2 Complete and stable financial inputs

- [ ] `p1` - **ID**: `cpt-cf-bss-subscriptions-dod-commercial-completeness`

The system **MUST** preserve stable economic identities and payer lineage, publish complete commercial descriptions with frozen known terms, explicit unresolved intervals and explicit removals, and seal the entire expected commercial set without taking ownership of money or usage completeness.

**Accuracy evidence:** all jointly approved golden scenarios must pass with zero lost or duplicated economic occurrences and complete exact interval/term evidence. Fixtures must contain independently specified expected Rating results and final Billing rounding for partial periods, price ends, corrections, suspension and zero replacement. Subscriptions supplies the source facts; its implementation must not reproduce the monetary evaluator to make the expected result agree.

**Implements**: `cpt-cf-bss-subscriptions-algo-recurring-evt`, `cpt-cf-bss-subscriptions-algo-traceability-evt`, `cpt-cf-bss-subscriptions-algo-dataset-priceoverride-evt`, `cpt-cf-bss-subscriptions-algo-seal-commercial-set`, `cpt-cf-bss-subscriptions-state-commercial-publication`.

**Touches**: accepted binding and effective-history stores, payer allocations, stable fact identities, Billing/Rating contract fixtures. Capability flags remain disabled for open S-U08/S-U13 one-time integration, S-Q04 schema and S-Q05 confirmation/correction integration paths despite accepted product directions.

## 6. Acceptance Criteria

- [ ] Redelivering the same fact/version/digest changes no commercial or financial identity; a changed digest under the same version fails visibly. Two legitimate revisions produce two recoverable immutable versions of one fact.
- [ ] A usage-only subscription publishes its parent at activation without waiting for usage or month end. Rating consumes that authoritative parent without synthesizing another obligation for the same source.
- [ ] A full October UTC usage parent permits Rating to derive 744 hourly slots; Subscriptions publishes one commercial parent per obligation and no hourly timer. A mid-hour activation clips served extent without inventing a full served hour.
- [ ] Closing a component after period opening publishes a replacement with exact covered term slices. A new PriceBook price alone changes neither component nor economic occurrence identity.
- [ ] Ten existing units under one binding plus two added units under a different accepted binding refuse before application while the S-U08 quantity-allocation contract is unavailable; they cannot become twelve units repriced at either tariff or overlapping temporal slices. Same-binding increases and distinct logical add-on items retain their supported paths. The initial joint fixture must reproduce each lot's frozen terms and complete quantity through increases, replay and correction.
- [ ] January 31 monthly anchoring clamps February and returns to March 31. A late authoritative cut/correction uses the original period payer; transfer is deferred.
- [ ] A crash before the atomic commit publishes nothing; a crash after it is repaired by reads even if all broker events are lost.
- [ ] A group containing recurring and an enabled one-time obligation cannot be sealed while that fact is missing. While exact S-Q04 schema or S-U08/S-U13 evidence is open, the fee-bearing profile refuses before capture/activation; S-Q03/S-Q04 approval cannot bypass the existing pre-capture safeguards.
- [ ] Given an approved qualifying occurrence with frozen illustrative EUR 50 setup terms and an existing EUR 50 advance allocated to that same obligation, one fact replays to the same EUR 50 rated result. Billing offsets the allocated advance without duplicate posting or another setup collection at activation. A partial advance reduces the remaining due; it does not waive or zero the tariff. The example sets no tariff or charge-trigger policy.
- [ ] Illustrative per-unit installation at EUR 10 for five units freezes Units(5, agreed unit) and rates EUR 50; later component growth to seven leaves it unchanged. An authorized correction of the original action to four units retains its occurrence and publishes a full successor fact/seal. Additional work creates another occurrence only with separate agreed chargeable action evidence. Flat Once is not multiplied by five; missing quantity, nonpositive quantity or incompatible model/unit is rejected rather than inferred.
- [ ] A one-time correction after catalog change retains the same charge occurrence, original agreed terms and explicit fact/result versions. Repeated delivery creates no extra obligation, posting or payment; Billing owns invoice correction and final rounding.
- [ ] A failed action retried under the same or another transport key/attempt, or recovered through a rebuilt draft, retains one purchase/charge occurrence. Resume and catalog price changes emit no new setup fee; S-U01 rebinding gates remain enforced.
- [ ] A new connection purchase after cancellation creates a distinct fee occurrence only when its agreed terms explicitly charge that action; without those terms it creates no setup fee. Independently agreed chargeable purchases at the same price remain distinct. Re-add, plan change, trial conversion and technical reprovision alone authorize no fee.
- [ ] A known October 20 end without authorized continuation produces v2 with the known prefix and explicit unresolved tail, plus an exact successor seal. Rating advances the fact head but emits no complete result; an older complete result cannot satisfy the new seal. Posted amounts are not automatically reversed. Resolution publishes full v3 and another seal; retries add neither versions nor charges. A missing initial admission binding still blocks sale/capture/activation.
- [ ] Removing an obligation publishes an explicit zero-obligation fact and successor seal; Rating returns a complete empty replacement (SUB-D-45). Billing removes only this fact's prior contributions through its normal guarded correction path; history remains and publication proves no refund completion. An active zero tariff or fully evaluated zero usage retains zero-valued classified/provenanced lines and required proofs. Missing terms/coverage stays structured pending, never empty/zero delivery; applicable minimum fees are still evaluated.
- [ ] Confirmed cutoff 10:00 and actual stop 10:05 preserves usage/runtime through 10:05 with no ordinary customer tariff for the technical delay. Unknown stop does not defer the known financial cutoff; physical history remains unresolved rather than fabricated.
- [ ] A wrong tail charge/posting/capture produces a versioned correction and Billing settlement of overpayment once, including on retry; original terms/attribution and allocated-advance history remain intact. Late pre-cutoff usage may remain chargeable under those terms, independently of report arrival.
- [ ] Authoritative cancellation/offboarding stop prevents ordinary accrual despite later scheduled cutoff, callback or deletion. Explicit retention terms can support their own obligation; presence alone cannot. Remaining confirmation/stop/closeout contracts remain gated and existing suspension policy is unchanged.
- [ ] Billing receives a complete commercial seal independently of Rating's complete window manifest. Missing either proof cannot be represented as a complete invoice input.
- [ ] No fixture passes by reading current prices, current payer or current composition in place of the frozen historical input.
- [ ] Under the ratified PRD §7 steady/burst workload, publication lag is measured against p95 < 5 s / p99 < 30 s; observed results are required before claiming the target.
- [ ] A deliberately missed opening or seal is discovered and durably repaired within the ratified 15-minute total budget from its due boundary, before the secondary daily audit; restart and competing workers preserve the same obligation identities.
- [ ] Required-provider outage retains a visible incomplete obligation and backlog age; elapsed time, an empty read or a daily audit cannot convert it into successful repair.
- [ ] Joint fixtures compare complete source facts with independently expected exact Rating outputs and final Billing rounding; every approved scenario passes with zero lost or duplicate economic occurrences.

- [ ] Flat/floor fixtures preserve distinct lot provenance without multiplying a same-term flat fee; an authorized mid-period change prorates by exact UTC seconds (SUB-D-46). Overlapping same-price lots share one minimum over union coverage; different price IDs retain separate floors. Subscriptions supplies inputs, not monetary calculation. Cross-fact floor enablement still requires its separate agreed protocol.
- [ ] Accepted terms retain the selected minimum scope (individual or group), window (calendar hour or billing period), calendar-hour alignment and full-or-proportional partial-period mode; replay reproduces the same floor (SUB-D-49, SUB-D-51, SUB-D-53, SUB-D-54).
- [ ] While the S-U08 quantity-allocation contract and runtime evidence are unavailable, the recurring 10+2 fixture refuses before commercial application. Once that gate is open, it publishes the same fact ID at version 2 with two temporal slices: A=10 before applied T and A=10 plus B=2 after T; the original ten retain their binding and both allocations reconcile to total twelve. No independent second total-quantity obligation appears.
- [ ] Replaying the purchase after response loss with a different transport key recovers its original lot/outcome; it creates no extra lot, quantity revision or fact version. A crash before local commit leaves none of these visible; a crash after commit recovers the committed outbox.
- [ ] Deliver version 2 twice, then version 1: Rating retains version 2 once. Conflicting content under version 2 is explicit; no stale delivery reprices the first ten or duplicates the last two. The successor seal requires version 2 before complete Billing consumption.
- [ ] Seal 7 names A/v4/d4; a complete result for A/v5/d5 arrives before seal 8. Billing stages it and cannot substitute it under seal 7. Seal 8 must name A/v5/d5 exactly; a mismatching digest conflicts even when the version number matches.
- [ ] With unchanged A/v4/d4, late usage produces a complete result r12 superseding r11. Billing can create a successor financial target under seal 7 with the exact new result/manifest refs; Subscriptions emits no new seal for this calculation-only change. An incomplete r12 or an already observed newer commercial head blocks target acceptance; unknown prior financial execution is recovered before applying the successor.
- [ ] An authorized correction retains fact/lot identities and frozen terms through full replacement; Billing replaces the draft or issues the supported posted correction once. Replaying financial completion never adds versions 1 and 2 as independent charges.
