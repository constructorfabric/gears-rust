---
description: "Cross-component product scope for separate commercial parties and single-subscription own-name resale, with usage-based and fixed-price recurring sponsored and multi-tier purchases in v1."
status: proposed
---

# PRD — BSS Commercial Model

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [Independent purchase authority — Q1](#independent-purchase-authority--q1)
  - [Chain topology and readiness](#chain-topology-and-readiness)
  - [Shared quantity changes](#shared-quantity-changes)
  - [Distinct parties in each sale — Q13](#distinct-parties-in-each-sale--q13)
  - [Optional bilateral negotiated terms — Q9](#optional-bilateral-negotiated-terms--q9)
  - [Purpose-limited commercial disclosure — Q6](#purpose-limited-commercial-disclosure--q6)
  - [Conditional initial purchase — Q5.1](#conditional-initial-purchase--q51)
  - [Initial financing and collection — Q12](#initial-financing-and-collection--q12)
  - [Active-chain obligations — Q5.2](#active-chain-obligations--q52)
  - [Per-leg usage valuation](#per-leg-usage-valuation)
  - [Account-level invoice grouping](#account-level-invoice-grouping)
  - [Sponsored commitment coverage](#sponsored-commitment-coverage)
  - [Independent financial attribution](#independent-financial-attribution)
  - [Action-specific commercial visibility](#action-specific-commercial-visibility)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [One service metered once, valued three times](#one-service-metered-once-valued-three-times)
  - [One service sold for 80 / 100 / 120](#one-service-sold-for-80--100--120)
  - [Independent funding and collection](#independent-funding-and-collection)
  - [Prepared purchase without activation](#prepared-purchase-without-activation)
  - [Renewal outside prior authority](#renewal-outside-prior-authority)
  - [Sponsor withdraws in month two of twelve](#sponsor-withdraws-in-month-two-of-twelve)
  - [Middle leg ends](#middle-leg-ends)
  - [Upstream disclosure](#upstream-disclosure)
  - [External sale versus internal chargeback](#external-sale-versus-internal-chargeback)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)
  - [Discussion decision index](#discussion-decision-index)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

**Selected channel baseline: resale in each intermediary's own name.** Each intermediary purchases from its supplier and sells onward in its own name. Each commercial transaction has its own seller/creditor, buyer/debtor, payer assignment, price, invoice and payment history. The target supports invoicing and settlement at every tier; agency and commission models are outside this baseline. The subscription model is one service subscription with a common period across tiers. Account use, party identity and tenant-scoped authorization follow the [ownership ADR](../../../gears/bss/docs/ADR/0002-cpt-cf-bss-adr-commercial-ownership-tenant-rbac.md); the chain model follows the [resale ADR](../../../gears/bss/docs/ADR/0003-cpt-cf-bss-adr-multi-tier-resale-model.md).

**v1 scope.** Usage-based consumption is the primary case: one service's metered usage is attributed once to the service and valued once per authorized leg grain under each leg's own tariff and assignment, with per-leg result identities, corrections and documents. Fixed-price recurring components with explicit quantity and term are valued the same way from the shared quantity. Independent commitment periods, decrease documents and cross-currency re-billing are outside v1. Every requirement here is proposed; none describes an implemented feature, and existing single-leg usage billing of the gears is not changed by this proposal.

### 1.2 Background / Problem Statement

A resource tenant can consume services sold and paid for by different organizations, so organizational ancestry cannot describe commercial relationships. Tenant management and platform authorization are independent platform capabilities that work without BSS; BSS consumes them and supplies its own domain facts. Account Management (AM) stays the source of truth for tenant identity, lifecycle and hierarchy and gains no payer/seller relationship entities, commercial workflows or dependency on BSS. AM's billing actor permits hierarchy aggregation under a barrier policy for reporting; it does not express BSS commercial ownership, and this proposal promises no group-wide cost view and no barrier bypass for users. BSS service principals traverse barriers deliberately only inside named internal service operations (DESIGN §3.3), never in a user-facing read.

### 1.3 Goals (Business Outcomes)

Support distinct service consumers, commercial buyers and payers; invoice each own-name resale transaction independently; avoid duplicate delivery and unauthorized disclosure; admit no purchase without durable account-use authority. This is a cross-component scope, not a new orchestration gear or a replacement for gear PRDs.

### 1.4 Glossary

Commercial leg = one seller–buyer transaction. Service subscription = the single resource-owned service lifecycle. Chain = the finite ordered list of legs of one service for one interval, frozen as a **chain version** by Subscriptions. Billing assignment = the account attributed to one leg for an interval, evidenced by an `AccountUseGrant`. Accounts terms (LegalParty, BillingAccount, BillingArrangement as offer plus acceptance, AccountUseGrant) are defined in the [Accounts PRD](../../../gears/bss/accounts/docs/PRD.md).

## 2. Actors

### 2.1 Human Actors

#### Commercial buyer

**ID**: `cpt-cf-bss-commercial-model-actor-buyer`

Authorizes its own purchases, independently of service recipient and payer.

#### Commercial seller

**ID**: `cpt-cf-bss-commercial-model-actor-seller`

Sells in its own name and remains responsible for its sale under accepted terms.

#### Payer representative

**ID**: `cpt-cf-bss-commercial-model-actor-payer`

Authorizes funding and reads permitted attributed financial data.

#### Resource tenant representative

**ID**: `cpt-cf-bss-commercial-model-actor-resource`

Manages its service and authorizes required disclosure under its own permissions.

#### Debtor representative

**ID**: `cpt-cf-bss-commercial-model-actor-debtor`

Reads the debt and documents of its legal party even when another payer funds them.

### 2.2 System Actors

#### BSS consumers

**ID**: `cpt-cf-bss-commercial-model-actor-services`

Orders, Workflow, Subscriptions, Accounts and the financial gears enforce their separate responsibilities.

## 3. Operational Concept & Environment

The [architecture manifest](../../ARCHITECTURE_MANIFEST.md) and the [ToolKit baseline](../../toolkit_unified_system/README.md) apply. This cross-component document selects no new runtime component.

## 4. Scope

### 4.1 In Scope

**Multi-tier billing: one subscription, a common period, a linear chain.** The baseline is successive purchases and sales in each intermediary's own name, Provider → Distributor → Reseller → Customer, evaluated separately from corporate consolidation and from transfer of billing responsibility; none of these is inferred from the AM tree. Ten licences at 8, 10 and 12 per licence per month produce three obligations of 80, 100 and 120 with separate seller–buyer/debtor pairs and payer assignments; no 300 balance exists anywhere, and an intermediary's receivable is not its payable.

**Funding.** A BillingArrangement covers eligibility to use an account for a named commercial buyer; it grants no resale right, upstream commitment, onward billing or transitive consent. Sponsors are not chain participants and may fund several legs.

**Usage and recurring valuation per leg.** The service is metered once. Each leg's tariff (its price binding or accepted price rule, units, window, aggregation, tiers, rounding, minimums, discounts, currency) is applied to that leg's authorized contribution grain and produces separately attributed results, documents and postings per leg — compatible lines from different subscriptions may share an invoice under the account-level grouping rule below, while each line retains its leg and no upstream obligation becomes part of a downstream debt; legs whose metering requirements are explicitly compatible may share one measured basis as an optimization, measured before any leg-specific pricing logic. Fixed-price components apply each leg's price to the shared quantity.

### 4.2 Out of Scope

Independent upstream commitment periods, separately renewing commercial subscriptions and wholesale pools are deferred; incompatible terms fail admission rather than being shortened. The common service period, the commitment term, invoice grouping cadence and payment due dates are distinct: different due dates are allowed (Q12), deliberately accumulating multiple billing periods of one obligation for quarterly or annual invoicing and new advance instruments are outside v1. Different service periods on separate lines and late-arriving charges in a later invoice are allowed; ordinary charge timing still follows accepted terms. Internal cost reporting fabricates no commercial leg. Decrease documents and cross-currency re-billing are outside v1. A leg whose tariff cannot be rated from the service's usage type is not ready, which denies that chain version rather than the usage model. Sales where the payer of a leg is that leg's seller are excluded as an initial scope choice pending explicit seller-funded semantics; this is not a claim about netting.

## 5. Functional Requirements

### Independent purchase authority — Q1

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-purchase-authority`

**Selected upstream purchase rule (Q1).** An upstream purchase may be admitted automatically only within explicit prior authorization from the commercial buyer of that leg, effective and applicable to the supplier, offer, quantity, currency, price rule, term and executing workflow; otherwise an authorized buyer confirms that specific purchase version. Confirmation bypasses no supplier eligibility, payer authority or admission check; a customer checkout triggers evaluation and impersonates no intermediary.

Three independent conditions hold on every leg: buyer purchase authorization, supplier acceptance under its policy, and account-use authority recorded as an `AccountUseGrant`. Prior buyer authorization automates only the first. Renewals are automatic only when prior authorization explicitly covers renewal within agreed bounds; a price outside them needs fresh confirmation. Each party withdraws its standing authority prospectively; withdrawal erases no incurred obligation and grants no termination right. Aggregate spending caps need an enforceable reservation or accounting contract, not a static field. Activation requires every leg of the chain version to pass its conditions; no distributed atomicity is promised.

**Actors**: `cpt-cf-bss-commercial-model-actor-buyer`, `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-payer`

### Chain topology and readiness

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-chain-topology`

A service has one chain per interval, frozen as a chain version by Subscriptions: a finite ordered list of legs where each leg's buyer is the next leg's seller and the last leg's buyer is the service's commercial buyer, who may differ from the resource tenant (the resource tenant stays validated service lineage). Chain participants — sellers and buyers — are pairwise distinct, so no cycle or repeated participant is possible; sponsors are not participants and may repeat; the payer of a leg is not its seller in v1. **Legal-party continuity:** at each adjacent boundary, the upstream leg's buyer/debtor party and the downstream leg's seller/issuer party must be the same verified legal party under Accounts' identity comparison, in addition to matching the intermediary tenant. Pin the comparison evidence and original party IDs on the chain version. Distinct or undetermined parties make that chain not ready; one tenant representing both EU GmbH and UK Ltd supplies no inter-entity transfer authority. Inter-entity supply transfer remains outside v1. **Required legs** are exactly the legs of the admitted chain version. A chain version is admitted only when every leg is ready: accepted purchase version, supplier acceptance, grant, compatible terms (Q5.2) and compatible billing basis. Readiness is an evaluation outcome recorded by Workflow and Subscriptions, not a new subscription status.

A **supply gap** — a leg that is missing, ended or unready at the next boundary — denies readiness for the next chain version and invokes explicitly authorized remedies: a validated replacement leg with all required agreements, or the affected seller's own contractual actions toward its buyer through authorized operations. It causes no automatic downstream termination, grace, service extension or upstream cancellation; existing commitments survive under their own terms. The source of the chain version is not exposed to resource-tenant users.

**Actors**: `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-buyer`, `cpt-cf-bss-commercial-model-actor-services`

### Shared quantity changes

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-quantity-change`

One logical shared quantity exists per service. An increase requested on the retail leg changes the quantity only after every leg of the chain version has a confirmed conditional preparation (buyer authority, supplier acceptance, grant for the delta); a denial leaves the quantity unchanged and cancels the prepared deltas through their authorized cancellation path with compensation under terms. A timeout is an unknown outcome to reconcile by identity before any cleanup or retry; it is not a failure. No atomic supplier commit is promised. A decrease document is unsupported in v1, and cancellation is not a partial-decrease workaround.

**Actors**: `cpt-cf-bss-commercial-model-actor-buyer`, `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-services`

### Distinct parties in each sale — Q13

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-sale-parties`

**Selected commercial-sale scope (Q13).** A commercial leg requires different seller and buyer tenants and different confirmed issuer and debtor legal parties, compared by verified party identity as defined in the Accounts PRD: different tenant ids or profile records prove nothing, and an undetermined identity supports no admission. Sales between legal parties of one tenant and transactions within one legal party are outside this model. Buyer = payer and buyer = resource tenant remain valid when the sale parties satisfy this rule.

**Actors**: `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-buyer`

### Optional bilateral negotiated terms — Q9

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-contracts`

**Selected Contracts party boundary (Q9).** A commercial agreement covers one seller/issuer legal party and one buyer/debtor legal party with their tenant references; master agreements covering several debtor parties are deferred. One agreement may govern many eligible purchases. It may restrict eligible payer accounts; such restrictions constrain financing without making the payer a party or recording its consent — Accounts remains authoritative for account usability and BillingArrangement consent. An order or line may omit a Contracts reference; all buyer, supplier, account-use and disclosure checks still apply, so the 80/100/120 chain needs no negotiated contract per sale. Contract signature, buyer purchase authority, supplier acceptance, payer consent and delivery acceptance stay distinct; a clause waiving separate acceptance is not Q1 authority. Contracts' present seller–payer party model requires revision to seller/issuer–buyer/debtor; it remains design-only.

**Actors**: `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-buyer`

### Purpose-limited commercial disclosure — Q6

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-disclosure`

**Selected upstream disclosure principle (Q6).** Access through an upstream commercial role exposes the permitted facts of that recipient's own sale or purchase, its direct counterparty and a scoped service reference; end-customer identity is excluded by default and disclosed only for a specified purpose, to an authorized recipient, within fields covered by the resource tenant's accepted terms or separate authorization. Offers requiring disclosure that the applicable authority does not cover are ineligible; data is never exposed first. A draft or unconfirmed payer identifier authorizes nothing. Lists, expansions, exports, progress reports and recipient-facing events follow the same rules; logs and events are no bypass. Withdrawal of disclosure authority stops further disclosure and terminates no service; retained documents follow their own access and retention rules. Access to a leg reveals no other tier's prices and grants no administration of the end customer's resources.

**Actors**: `cpt-cf-bss-commercial-model-actor-resource`, `cpt-cf-bss-commercial-model-actor-seller`

### Conditional initial purchase — Q5.1

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-conditional-purchase`

**Selected initial-purchase restriction (Q5.1).** The initial automated chain supports only offers whose terms create no irreversible payment obligation before the agreed, confirmed service activation. A prepared or conditionally accepted purchase may exist before activation under the authority required for that preparation (a quote or a conditional, cancellable supplier acceptance); what may not happen before the grant and the common trigger is incurring, purchasing or activating. Before the trigger, every prepared purchase must be cancellable under its agreed conditions through a separately authorized cancellation path without a non-refundable charge; offers that cannot satisfy this are rejected from the automated flow.

**Actors**: `cpt-cf-bss-commercial-model-actor-buyer`, `cpt-cf-bss-commercial-model-actor-seller`

### Initial financing and collection — Q12

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-initial-financing`

**Selected initial-purchase payment sequence (Q12).** Validate financing before service activation and collect after confirmed activation according to each leg's payment terms; checkout that captures funds before service is deferred. Before common activation each leg has Q1 authority, supplier acceptance, a grant and applicable financing validation; a grant is neither a payment, a balance, a credit approval nor a guarantee of collection. An unresolved financing result is not success. Service may be active when collection fails: preserve the actual outcome, the debtor and the assignment; follow the sale's payment terms, due date and recovery policy under the **existing** payment and dunning semantics of the gears; a failed collection is not automatically an overdue debt, permission to suspend the chain, a charge to another party or collection from another account. Unknown outcomes are reconciled idempotently; a timeout proves nothing. Earlier advances and receipts keep their evidence and restrictions. Payments owns authorization, collection and reversal; Invoicing documents; Ledger balances and allocation; Workflow process progress.

**Actors**: `cpt-cf-bss-commercial-model-actor-payer`, `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-debtor`

### Active-chain obligations — Q5.2

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-active-obligations`

**Selected active-chain responsibility and admission policy (Q5.2).** Each seller remains responsible for its downstream sale under that sale's accepted terms; an upstream failure, price increase or non-continuation releases no downstream obligation and authorizes no termination of other legs or the service. Admit only chains whose commitments, notice deadlines and renewal windows are compatible with the common service period; upstream commitments must cover the downstream promise and upstream notice must leave time for permitted downstream actions. Reject incompatible chains; an intermediary's acceptance of supply or financial risk is not an enabled exception. Failure to obtain fresh upstream authority prevents admission under that authority and proves no discharge; the one subscription and shared-period coordination are kept, Subscriptions owns the lifecycle outcome, Workflow coordinates required confirmations, and a supply or authority gap is distinguished from the debtor's nonpayment and from payer failure, each keeping its existing handling. Compatible commitments guarantee no supplier performance; incidents are represented truthfully, and no cancellation cascade follows. Shared-boundary coordination is contract-conditioned: requests may be refused, transitions preserve accrued liabilities, and upstream termination never erases downstream obligations.

**Actors**: `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-buyer`, `cpt-cf-bss-commercial-model-actor-services`

### Per-leg usage valuation

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-usage-valuation`

Metered consumption of the one service is attributed once to the service and then valued **once per authorized leg grain**: for each leg of the chain version, each usage record contributes exactly once to that leg's counter under that leg's pinned tariff (meter, unit, window, aggregation), and the leg's result carries its own identity, seller, payer account, grant, debtor party and book. The valuations of one record for exactly the validated legs of the admitted chain version are intentional fan-out; they are neither quarantined as ambiguous nor collapsed into one economic obligation. Everything else keeps the existing rejection and quarantine rules: missing, overlapping or conflicting source subscription segments, duplicate leg ownership, and chains that are unauthorized or whose version does not match the segment still reject or quarantine. Legs whose metering requirements are explicitly compatible may share one measured basis as an optimization, measured before tier, rounding, minimum, discount and currency logic; a retail priced result is never reused upstream. Corrections and reversals re-value every affected leg under the same revision; retries re-count no leg; a leg whose binding is missing stays pending for that leg while other legs proceed. Usage spanning a window or assignment boundary is split under the metric's supported window rules before valuation and never assigned whole to one assignment; late-arriving usage resolves to the chain version and assignments of its own window, including when the former payer's account is since blocked or closed. Usage is billable only with consumption-time evidence from its source: the event time for a point type or the exact window for an aggregate, never the arrival time alone. Missing evidence is restored exactly from the source where possible; until then the usage is a visible unresolved exception; if it cannot be restored, an explicit approved source-data-loss decision with evidence and scope closes the gap, and no charge is estimated. Such a decision is not zero consumption, a write-off or a release of existing obligations. No customer debt is summed across wholesale legs. Admission requires accepted, pinned price coverage for every declared billable component of every required leg, including the supported meter mappings for usage components; a missing price is never implicit inclusion, a zero rate or permission to copy another seller's price.

**Recovered data after accepted loss (selected for v1).** Data recovered after an approved `accepted_loss` does not automatically create or change a charge. Validate source provenance, exact consumption time and scope, and ensure the recovered measurements do not duplicate contributions already counted. An authorized operator confirms an administrative re-rate with an audited reason, evidence, affected interval and legs. Only that confirmed scope enters normal rating and financial correction processing, under its historical tariff and assignment; existing downstream approval, tax and lifecycle checks still apply. Unverified or unapproved recovery stays a visible exception. The original loss decision and recovery evidence remain retained; accepted loss is neither zero usage nor a debt write-off. This extra confirmation applies to recovery after accepted loss, not ordinary late usage. Collector retention/backfill limits are not bypassed by the confirmation.

**Usage finalization (selected for v1).** Use Rating's explicitly configured `delay_only` policy with recorded Finance/Product acceptance by an authorized approver distinct from the policy writer. The delay must be configured for the applicable seller or platform policy; this proposal selects no numeric delay and does not change Rating's general `full` default. After the window and its delay, results may finalize without proof that every source record has arrived. Known open source losses still block the affected scope, and unresolved pricing, attribution or other required processing is not waived. Each leg delivers its result only when all its expected windows are final; late eligible measurements and corrections produce new revisions of the same economic obligation under the original event-time attribution. Issued invoices remain immutable; the financial owner derives any credit or debit adjustment. Rating finalization neither issues an invoice nor closes a Ledger period. Invoice grouping follows the selected rule below; its execution remains owner integration work. **Late corrections after period close (selected for v1):** the normal route is a new correcting document and compensating entries in the current open accounting period, where Ledger policy permits. The original service period, seller, debtor, payer account, grant, assignment, currency and book attribution remain attached; the accounting posting period does not replace consumption time. A correction to an issued amount uses the financial owner's credit/debit adjustment, not another full invoice for the revised amount. An unbilled late result is a first invoice, not a correction to a nonexistent document. No automatic reopening, backdating or edit of a closed period is allowed. If no eligible open period exists or a lifecycle, tax, approval or posting rule prevents the operation, retain a visible exception for Finance; the routine worker cannot bypass it. Any exceptional reopening follows Ledger's separately authorized governance. This selects a business route, not an implemented cross-gear correction flow.

**Terms used for billing (selected for v1).** Billing uses the effective commercial and payment terms retained for the accepted purchase version. Applicable accepted terms override account defaults; defaults only fill permitted omissions. Neither a default edit nor invoice grouping changes admitted terms, and negotiated terms never extend payer consent or override account bindings. Changed terms require the supported amendment/renewal authority and a matching grant. Consumers retain the source versions and resolved values; unresolved conflicts fail admission.

**Actors**: `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-payer`, `cpt-cf-bss-commercial-model-actor-services`

### Account-level invoice grouping

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-invoice-grouping`

**Selected for v1.** Consolidate ready, due-to-be-invoiced charges at the BillingAccount's configured invoice-cycle cutoff. Lines from independent subscriptions may share one invoice when the account, payer, seller, issuer party, debtor party, billing currency and Ledger book match, and payment terms, tax rules and recipient disclosure permit it. Each invoice has one payment due date in v1. Lines may be combined only when their due dates, derived under the accepted terms, match; otherwise issue separate invoices. Grouping never moves an existing due date or changes the contractual basis for calculating it. Different payment deadlines on individual lines of one invoice are outside v1. Matching service-period boundaries are not required: the common period applies to the legs of one resale chain, not to all subscriptions using an account. Each line preserves its own service period, subscription, leg, assignment, grant and source result/revision; grouping creates no new obligation and transfers none between parties. A subscription may explicitly request a separate invoice for its affected leg; this preference changes no other leg's invoicing. Other custom grouping keys, such as PO or contract groups, are deferred.

A result that is not final or ready at the cutoff stays a visible exception and does not hold other ready results. Once eligible, an unbilled result is included in a later run under its original financial attribution; an already billed result requires the financial owner's correction flow, not another full charge. Known source losses and unresolved pricing still block the affected result. Grouping never changes accepted payment terms or authorizes withholding a required invoice; the owner contract must handle incompatible timing separately. Issued invoices remain immutable. Multiple accounts may be summarized in a payer statement, but v1 does not consolidate them into one invoice or offset their obligations. Accounts stores the account's invoice-cycle preference; Invoicing validates compatibility and owns scheduling, document generation and corrections. This defines required behaviour, not a delivered Invoicing gear.

**Actors**: `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-payer`, `cpt-cf-bss-commercial-model-actor-services`

### Sponsored commitment coverage

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-sponsored-coverage`

A sponsored commitment longer than one billing period is admitted only when the payer's grant covers the full commitment term **before** the obligation is incurred; a mismatch refuses the use or lets the buyer choose an already-defined alternative such as its own account. The covered term is the accepted product term under the product's frozen rules; v1 has no separately configured calendar funding end, and the calendar interval of an initial purchase follows the actual service start without backdating. For usage components the grant authorizes the tariff and the consumption policy of the Accounts PRD, not a final amount: under `accepted_variable_under_tariff` the payer accepts the amounts actual consumption produces under the pinned tariff; under `quota_controlled` a Subscriptions usage-quota entitlement enforced at the point of use by the operational support system bounds further usage within the specified block-or-degrade behaviour, units and scope, while usage already incurred, including within the rating lag and any overshoot, is attributed and invoiced — a quota is not a guaranteed financial cap; an offer selecting a quota mode or behaviour that the product's entitlements do not provide fails at setup or admission, not the metered purchase as such; variable amounts are funded only under an explicitly accepted tariff policy. Revoking the arrangement stops new uses and leaves the authorized exact use and its coverage intact. Coverage ends legitimately through cancellation or termination of the purchase under its terms, a completed transfer to another eligible account of the same debtor party, or the end of the service; the end of funding authorization erases no remaining obligation, including termination debt. In v1 a planned payer or account transfer of a leg takes effect only at a shared period boundary and only when the replacement account is authorized by then; a transfer inside a period is deferred, an unready replacement extends no old authority, and no transfer reassigns past usage or documents. The debtor has an authorized view of its debt and documents even while another payer funds them; nothing is debited from the debtor's instruments automatically; payment-instrument permission is a separate, revocable contract; Ledger determines the debtor's and payer's financial consequences.

Under `quota_controlled`, the payer explicitly accepts the pinned tariff for eligible actual usage and the recorded rating-lag/overshoot exposure, in addition to selecting the quota enforcement. The same acceptance and exposure are retained in the offer, accepted terms and grant. A quota is not a financial cap; consumed usage within that accepted exposure remains funded and invoiceable even if enforcement or rating arrives later. Absent explicit acceptance, admission fails; neither the quota nor resource scope creates funding authority.


**Actors**: `cpt-cf-bss-commercial-model-actor-payer`, `cpt-cf-bss-commercial-model-actor-debtor`, `cpt-cf-bss-commercial-model-actor-buyer`

### Independent financial attribution

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-independent-finance`

One subscription implies no single payer, price, invoice or payment. Each leg keeps its billing assignment, grant evidence, issuer party, debtor party, Ledger-resolved book, documents and history; payment status, credit notes and refunds attach to the affected leg. Cancelling the subscription erases no accrued obligation and refunds nothing automatically. Each financial operation uses its own idempotency and recovery contract; a shared period is not an atomic transaction across books. The commercial buyer stays distinct from the resource tenant; `resourceTenantId` is not redefined. Compatible accounts share a Ledger settlement pool only on all compatibility dimensions (payer tenant, debtor party, seller tenant, issuer party, book, billing currency); no universal payer balance or automatic offset across suppliers exists.

**Actors**: `cpt-cf-bss-commercial-model-actor-payer`, `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-debtor`

### Action-specific commercial visibility

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-fr-visibility`

| User and tenant scope | Intended access, subject to explicitly granted permissions | No automatic access to |
|---|---|---|
| Customer employee acting as buyer and resource-tenant representative | Its authorized purchase orders and service subscriptions, including services paid for by another tenant | Another payer's unrelated billing data or spending authority |
| Commercial buyer employee in an intermediary tenant | Its own upstream orders and permitted terms under buyer permissions | Other buyers' orders, downstream resource administration, unrelated payer data |
| Seller operator in the seller tenant | Orders and subscriptions sold by that seller, for the actions permitted to that role | Customer resources, service administration, another seller's sales |
| Finance employee in the payer tenant | That payer's accounts, offers, grants, attributed charges and documents, and permitted details of related orders and subscriptions | All services of a resource tenant or resource administration |
| Debtor representative in the debtor party's representing tenant | The debt, documents and payment status of its legal party's obligations, whoever funds them | The payer's other accounts or the service's operational data |

The payer path spans the accounts of that payer tenant; account labels, filters and groupings are no confidentiality boundaries. Hierarchy-derived access follows existing platform policies with `self_managed` barriers respected: payer and seller relationship paths never expand to ancestors by default; an independent customer/resource-subtree permission held by a managed parent remains its own path (Holding as ACME's managed parent may see ACME-paid Backup through it, never through the payer path, and not at all when ACME is self-managed); optional customer-cost reporting uses resource-scoped permitted expense data with upstream prices excluded and views deduplicated, not a subtree query over payer-owned views; no blanket ancestor access or user barrier bypass is granted, and the deliberate barrier traversal of BSS service principals inside internal service operations creates no user-visible reporting path. A requested payer or account on a draft creates no authorized reader; payer, seller and debtor paths use assignments and grants validated by the owning gears. Prices visible to the customer are not secret merely because another tenant pays; paying an invoice justifies no operational read; a historical document's source reference grants no access to the current source object. Payer billing views need explicit action permissions; field restrictions apply to lists, details, counts, exports and nested expansions; filtering rows through the PDP does not replace the permitted response schema, and a billing permission never promotes a partial view into a full read.

**Actors**: all human actors above

## 6. Non-Functional Requirements

### 6.1 Gear-Specific NFRs

#### Commercial isolation

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-nfr-isolation`

Every detail, list, count, expansion and export preserves the authorized recipient's leg and field boundary; an object reference, payer assignment or ancestor relationship reveals no other tier's prices and permits no resource administration. Verification: negative cases across all read surfaces.

#### Repeat-safe financial attribution

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-nfr-replay`

Retries, late and replayed events, usage records and corrections preserve the original leg, chain version, grain and assignment, producing no additional service, no duplicate contribution to any leg and no collapse of distinct leg valuations; unknown outcomes stay distinguishable from confirmed failure. Verification: replay traces before release.

#### Fresh current-view validation

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-nfr-fresh-current-views`

Current views derived from consent, grant or assignment facts (payer purchase views, payer leg views, debtor document views, offer summaries) are served only after fresh authoritative validation of their basis for the request, list page or export batch, batched by basis, with no positive disclosure cache and with PDP evaluation per request, page or batch; unavailability of the authoritative source withholds the affected current response. The historical financial slice of authorized uses is separately authorized and stays available; historical documents may still receive append-only payment and correction events. Verification: revocation concurrent with reads, delayed and out-of-order events, source outage, pagination and totals.

#### Tenant-count-independent operation scopes

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-nfr-target-scoping`

Each purchase, renewal, projection, financial-status and pinned-price call requests only its explicit target tenants, using one PDP decision per target/action/resource and enforcing a singleton owner scope. The deployment verification target is 100,000 tenants; test at 10,000 and 10,001 to exclude a hidden dependency on whole-tree expansion limits. This is a proposed acceptance envelope, not evidence of measured capacity. The owner predicate remains constant-sized as the platform grows; provider latency/capacity still needs verification. Above the tested envelope the same target-scoping semantics apply, with ordinary dependency backpressure/failure for affected calls, not platform-wide refusal based on tenant count.

The 200-ID batch limit bounds each status request; partition it by authoritative owner and process with bounded concurrency. Autonomous renewal, outcome-reconciliation and projection-rebuild discovery uses bounded paginated work/tenant references and checkpoints, followed by single-target data access. No whole-platform BSS payload scope is a v1 exception. Production-provider confirmation must cover action/role enforcement, self-managed targets, returned-scope validation and target lifecycle before ADR acceptance.

### 6.2 NFR Exclusions

No global security or reliability exemption is selected; there is no distributed atomic activation, payment or revocation guarantee across databases.

## 7. Public Library Interfaces

### 7.1 Public API Surface

This product composition exposes no facade SDK; Orders, Accounts, Subscriptions and the financial gears own their interfaces.

### 7.2 External Integration Contracts

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-contract-commercial-evidence`

Each producer provides its consumers versioned purchase, party, price, chain-version, grant and assignment evidence sufficient to enforce this scope. The [DESIGN](DESIGN.md) specifies the proposed owner contracts; none is a delivered wire API.

## 8. Use Cases

### One service metered once, valued three times

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-usecase-usage-resale`

One compute service of ACME consumes 720 VM-hours in a month. Provider sells to Distributor at 0.10 per VM-hour (72.00), Distributor to Reseller at 0.125 (90.00, funded by Holding), Reseller to ACME under a graduated tariff of 0.16 for the first 500 hours and 0.12 beyond (106.40). The usage is attributed once to ACME's service; each leg's grain counts each record once under its own window and meter; three results with three obligation identities and separately attributed receivables exist, each leg's documents grouped only within that leg. A re-rate that corrects the month to 700 hours re-values all three legs under one revision; a record spanning the month boundary is split before valuation; a record arriving three weeks late resolves to its own month's chain version and assignments.

**Actor**: `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-payer`, `cpt-cf-bss-commercial-model-actor-buyer`

### One service sold for 80 / 100 / 120

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-usecase-resale`

Fixed recurring variant: ten licences per month. Provider sells to Distributor at 8 per licence (80), Distributor to Reseller at 10 (100, funded by Holding), Reseller to ACME at 12 (120). Reseller's prior authorization covers up to 40 licences at no more than 10 per licence; ten pass, fifty would need confirmation. Only the service order creates the service subscription; upstream orders attach legs; the chain version is [L1, L2, L3]. Each buyer sees its authorized order, each seller its sale, Holding the payer view of L2; ACME sees no wholesale leg. Three separately attributed receivables exist; no 300 balance does.

**Actor**: `cpt-cf-bss-commercial-model-actor-buyer`, `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-payer`

### Independent funding and collection

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-usecase-financing`

ACME's card purchase follows authorization, confirmed activation and then collection; a net-30 purchase validates postpaid conditions, activates and is invoiced under those terms. In the chain, Distributor may be postpaid, Holding may fund L2 by another approved method and ACME may pay by card; each leg passes its own financing check before common activation; collection dates are not synchronized.

**Actor**: `cpt-cf-bss-commercial-model-actor-payer`

### Prepared purchase without activation

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-usecase-failed-activation`

Provider conditionally accepts Distributor's prepared purchase for 80; Distributor is ready to sell to Reseller for 100; Holding refuses the grant for that use. The chain is not ready; the ACME service is not activated; Workflow requests cancellation of the prepared purchases through their authorized cancellation paths and tracks results. Distributor owes no non-refundable charge only because Provider's terms were conditional. An offer requiring an irreversible payment at acceptance is rejected from the automated flow; its risk is assigned to no one silently.

**Actor**: `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-payer`

### Renewal outside prior authority

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-usecase-renewal`

Distributor proposes 110 instead of 100, outside Reseller's prior authority, while Reseller sells to ACME for 120. Reseller may seek new authority (with an independent Holding grant), arrange a validated replacement supply or exercise a permitted downstream right within its notice window; none is implicit. The platform raises no price, charges Holding no difference and migrates nothing without authority. A one-year downstream promise over a one-month upstream commitment is rejected under Q5.2.

**Actor**: `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-buyer`

### Sponsor withdraws in month two of twelve

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-usecase-sponsor-withdrawal`

Holding's grant for Reseller's twelve-month purchase from Distributor covered the full term at authorization. In month two Holding revokes its offer: new uses (renewal into a second year, increases) are refused; months three to twelve remain assigned to Holding's account under the issued grant; Holding keeps its authorized view of that obligation's evidence; Reseller GmbH remains the debtor; Reseller may transfer the leg to its own account from a shared period boundary, when that account is authorized by then. If Holding also revokes its instrument mandate, collection fails and the leg's existing payment and dunning semantics apply to the debtor and payer as Ledger and Payments determine.

**Actor**: `cpt-cf-bss-commercial-model-actor-payer`, `cpt-cf-bss-commercial-model-actor-debtor`

### Middle leg ends

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-usecase-supply-gap`

Reseller does not renew L2 at the boundary. The next chain version is not ready (supply gap at L2). Reseller may present a validated replacement leg with all agreements; otherwise the next period is not admitted and Reseller's remedies toward ACME run under L3's terms through authorized operations. L1's obligations survive under their terms; nothing terminates, extends or cancels automatically.

**Actor**: `cpt-cf-bss-commercial-model-actor-seller`

### Upstream disclosure

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-usecase-privacy`

For a capacity offer that needs no end-customer identity, Provider receives its buyer Distributor, product, quantity, period and a permitted service reference; Distributor receives its own sale facts. A named-licence offer requiring ACME's name and country receives them only when ACME's authorization covers purpose and recipient. Holding's offer may restrict sponsorship to ACME using trusted scope facts without receiving ACME's profile.

**Actor**: `cpt-cf-bss-commercial-model-actor-resource`

### External sale versus internal chargeback

- [ ] `p1` - **ID**: `cpt-cf-bss-commercial-model-usecase-legal-parties`

A seller-capable Reseller tenant buys from Provider and sells to external ACME; each leg has a distinct counterparty verified by party identity. EU GmbH and UK Ltd, two parties represented by Reseller, may sell to external buyers under their bindings; a sale from EU GmbH to UK Ltd is outside the model. A Q9 agreement between Distributor's party and Reseller GmbH may permit financing through Holding's account H-RES or Reseller's R-RES; switching between permitted accounts for the same debtor is a funding revalidation, not a new agreement.

**Actor**: `cpt-cf-bss-commercial-model-actor-seller`, `cpt-cf-bss-commercial-model-actor-buyer`

## 9. Acceptance Criteria

- AM is deployable without BSS or commercial-relationship storage; no authoritative copies or workflows are introduced into AM.
- Accounts owns party identity, accounts, consent and grants; Contracts owns negotiated terms.
- A third-party-funded order without a contract reference passes billing eligibility through an effective arrangement, a grant and a usable account, and still passes every other guard; a missing arrangement never falls back to platform defaults.
- No purchase, renewal, increase or transfer is admitted without durable grant evidence; a revoke committed before authorization refuses it and a revoke committed after leaves the exact use authorized.
- A customer reads permitted details of Compute and Backup although their payers differ; a Holding finance role with payer permissions only reads its Compute billing data and not Backup; the debtor reads its debt whoever pays.
- A draft naming an unconfirmed payer appears in no payer list; a valid relationship without a matching permission grants nothing.
- Under default consent an authorized buyer submits an in-scope purchase without payer approval; without purchase permission, out of scope or on an unusable account, admission fails; required per-use approval cannot fall back.
- A payer buying from two sellers or in two currencies uses distinct accounts; an arrangement for one account admits no purchase from another seller.
- Accounts compatible on all dimensions share settled funds under Ledger's policy; different debtors, payer tenants, books or currencies never share implicitly.
- After confirmation, an AM edit changes no billing profile; after issuance, a profile edit affects only later documents; debtor replacement is never a profile edit.
- Current payer, debtor and buyer views reflect revocation on the next request without a positive cache and are withheld when their authoritative source is unavailable; counts, pages and exports obey the same predicates.
- The 80/100/120 chain yields three obligations in three books from one quantity, and the 72/90/106.40 usage chain three obligations from one metered service, replay-safe, with one subscription, one period and one chain version; a correction re-values every leg; a leg with a missing binding stays pending alone; a chain with a cycle or a repeated participant, an incompatible commitment, an unratable leg tariff or an unverified party is refused.
- An increase denied on one leg changes no quantity and leaves no uncompensated prepared delta; a timeout is reconciled, not treated as failure.
- A sponsored twelve-month commitment is admitted only with full-term coverage; revocation in month two changes neither the authorized term nor the debtor.
- A supply gap denies the next chain version and terminates nothing; an upstream price or authority gap raises no downstream price, charges no sponsor and marks no one a nonpayer.

These are confirmation criteria for future implementation, not executed tests. The [DESIGN scenario checks](DESIGN.md#43-pre-acceptance-scenario-checks) are document-level analysis.

## 10. Dependencies

Local readiness is partial: the [Rating PRD](../../../gears/bss/rating/docs/PRD.md) describes a platform/partner/customer hierarchy; the [Ledger PRD](../../../gears/bss/ledger/docs/PRD.md) treats the seller as line metadata and defers inter-tenant settlement; Ledger's accepted book-ownership ADR derives payer resolution from the buyer hierarchy. Adding Accounts implements none of multi-tier invoicing or settlement. Orders and Workflow own purchase documents and processes; Subscriptions owns service timing, chain versions and assignment history; Accounts owns party identity and account-use authority; Pricing, Products and Contracts supply terms; Rating, Invoicing, Payments and Ledger own financial stages; the platform owns authorization; service commands and derived-view production use its existing service-principal identity and root-scoped role assignments as the DESIGN describes, and each deployment must provision the BSS service principals and roles before sponsored uses are served.

## 11. Assumptions

Suppliers support the declared conditional offer terms. Hierarchy proves neither resale authority nor legal-party identity. One subscription is not an atomic transaction across suppliers or books.

## 12. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Irreversible supplier acceptance before activation | Uncovered upstream liability | Reject incompatible offers; authoritative conditional terms |
| Use admitted without a grant committed before the revocation | Payer funds what it refused | One-transaction grant; admission only on grant evidence |
| Stale current views after revocation | Disclosure after consent ended | Fresh validation per request, page and batch; no positive cache |
| Shared usage or quantity loses leg identity | Quarantined, missing or duplicate liabilities | Chain-version-keyed per-leg grains and results |
| Sponsored usage without a declared policy | Unexpected exposure or a cap that only declines invoices | Explicit consumption policy; quota modes only where a pre-consumption entitlement exists |
| Common period confused with commitment or payment cadence | Unsupported combinations admitted | Separate terms, fact periods, grouping and due dates |

## 13. Open Questions

The v1 product rules and component contract choices are specified: account-level invoice grouping with individual line periods; one due date per invoice; approved `delay_only` finalization; corrections in a policy-eligible open period; controlled recovery after accepted loss; accepted terms over defaults with independent consent. [DESIGN §§3.8–3.11](DESIGN.md#38-orders-terms-and-subscriptions-contracts) fixes the component APIs, state machines, identities, transactions, failure handling and rollout; §4.4 fixes GTS schemas, extensions, events and permissions.

Deployment must supply confirmed legal-identity namespaces, seller accounting profiles, approved finalization/finance policies, service principals and capacity budgets. Owner acceptance, implementation and scenario/concurrency verification remain required; these are not unanswered interface choices. Custom invoice groups, cross-account consolidation, new advance instruments, transfer inside a period and automatic cached permission leases remain explicitly outside v1.

## 14. Traceability

- [Commercial DESIGN](DESIGN.md)
- [Ownership ADR](../../../gears/bss/docs/ADR/0002-cpt-cf-bss-adr-commercial-ownership-tenant-rbac.md)
- [Resale ADR](../../../gears/bss/docs/ADR/0003-cpt-cf-bss-adr-multi-tier-resale-model.md)
- [Accounts PRD](../../../gears/bss/accounts/docs/PRD.md)

### Discussion decision index

| Discussion | Canonical detailed home | Meaning |
|---|---|---|
| Q1 | [Purchase authority](#independent-purchase-authority--q1) | Prior scoped buyer authority or fresh confirmation |
| Q2 | [Accounts arrangement](../../../gears/bss/accounts/docs/PRD.md#consented-account-use--q2) | Account to commercial buyer; payer offer, buyer acceptance |
| Q3 | [Commercial domain model](DESIGN.md#31-domain-model) | Buyer-owned purchase orders with attachment legs |
| Q4 | [Component responsibilities](DESIGN.md#32-component-model) | Workflow purchase process; Subscriptions lifecycle and chain versions |
| Q5.1 | [Conditional purchase](#conditional-initial-purchase--q51) | No irreversible obligation before agreed activation |
| Q5.2 | [Active-chain obligations](#active-chain-obligations--q52) | Compatible terms; each seller's own responsibility |
| Q6 | [Disclosure](#purpose-limited-commercial-disclosure--q6) | Purpose and authority for upstream customer data |
| Q7 | [Accounts seller setup](../../../gears/bss/accounts/docs/PRD.md#seller-and-issuer-eligibility--q7-and-q11) | Issuer bindings; Ledger books and resolution |
| Q8 | [Party separation](../../../gears/bss/accounts/docs/PRD.md#buyer-debtor-payer-and-recipient-separation--q8) | Debtor distinct from funding, recipient and instrument |
| Q8.1 | [Account binding](../../../gears/bss/accounts/docs/PRD.md#account-binding-and-debtor-validation) | One debtor party per account |
| Q9 | [Contracts](#optional-bilateral-negotiated-terms--q9) | Optional bilateral seller/issuer–buyer/debtor agreement |
| Q10 | [Bilateral confirmation](../../../gears/bss/accounts/docs/PRD.md#verified-bilateral-confirmation--q10) | Independent authority; separate acts; one actor identity |
| Q11 | [Seller eligibility](../../../gears/bss/accounts/docs/PRD.md#seller-and-issuer-eligibility--q7-and-q11) | Configured platform/partner seller types |
| Q12 | [Initial financing](#initial-financing-and-collection--q12) | Financing before activation; collection afterward |
| Q13 | [Sale parties](#distinct-parties-in-each-sale--q13) | Distinct tenants and verified distinct parties |
