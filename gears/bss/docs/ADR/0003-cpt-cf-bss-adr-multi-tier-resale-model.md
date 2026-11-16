---
description: "Represent own-name resale as buyer-owned purchases and seller-owned commercial legs around one shared service subscription with a common period."
status: proposed
date: 2026-11-08
---

# Represent Multi-Tier Resale as Commercial Legs of One Service Subscription

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [A. One service subscription with seller-owned commercial legs and buyer-owned purchase orders](#a-one-service-subscription-with-seller-owned-commercial-legs-and-buyer-owned-purchase-orders)
  - [B. Independent subscription lifecycles for every commercial tier](#b-independent-subscription-lifecycles-for-every-commercial-tier)
  - [C. Resource-owned legs, or an unowned aggregate with custom party-axis authorization](#c-resource-owned-legs-or-an-unowned-aggregate-with-custom-party-axis-authorization)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-adr-multi-tier-resale-model`

## Context and Problem Statement

Own-name resale creates several commercial sales for one delivered service: a provider sells to a distributor, the distributor to a reseller, the reseller to the customer. Each sale has its own buyer authority, price, debtor, payer assignment, invoice and payment history, while the service is delivered and metered once. Decide how to represent the sales without duplicating provisioning, consumption or service lifecycle, and which component coordinates purchase-driven processes. Account use and tenant-scoped authorization follow the [ownership ADR](0002-cpt-cf-bss-adr-commercial-ownership-tenant-rbac.md).

## Decision Drivers

- Preserve each intermediary's independent purchase and sale and its private terms.
- Deliver and meter the service once.
- Keep existing document, process, service and money responsibilities explicit.
- Admit only chains whose topology, terms and billing basis the platform can actually evaluate.
- Preserve obligations and truthful outcomes through partial failure, supply gaps and timeouts.

## Considered Options

- **A. One service subscription with seller-owned commercial legs and buyer-owned purchase orders.**
- **B. Independent subscription lifecycles for every commercial tier.**
- **C. Resource-owned legs, or an unowned aggregate with custom party-axis authorization.**

## Decision Outcome

Proposed option: **A**. One resource-owned service subscription with a common period and lifecycle carries a finite, linear **chain** of commercial legs. Each leg is owned by its seller tenant; each buyer owns its versioned purchase order (Q3); the commercial buyer of the retail leg may differ from the resource tenant, which remains validated service lineage. Upstream orders attach commercial obligations to the same service and never spawn subscriptions, provisioning or usage streams. Required legs are exactly the legs of the admitted **chain version**; chain participants (sellers and buyers) are pairwise distinct, so there are no cycles; funding sponsors are not chain participants and may repeat.

Orders owns purchase intent and accepted versions; Orders Workflow coordinates purchase and fresh-confirmation processes; Subscriptions owns the service lifecycle, renewal timing, chain versions and effective assignment history (Q4). Orders Changes stays the increase-document path. Buyer purchase authority (Q1), supplier acceptance and account-use authority are three independent conditions on every leg; read permission and process ownership confer no mutation authority.

**v1 scope:** usage-based and fixed-price recurring sponsored and multi-tier purchases. Usage is the primary case: one service's metered consumption is attributed once to the service and valued once per authorized leg grain under that leg's own tariff and assignment, producing separately attributed results, corrections and financial document lines per leg. Invoicing consolidates compatible ready lines within one BillingAccount at its configured invoice cutoff; lines retain their own service periods and leg identities, a subscription may request separate invoicing for its leg, and an unready leg does not delay another. Shared invoices have one contractual payment due date; different dates require separate invoices. Grouping requires compatible parties, currency, book, payment terms, taxes and recipient disclosure; cross-account aggregation is reporting only (720 VM-hours at 0.10, 0.125 and a graduated retail tariff yield three different obligations; 10 licences at 8/10/12 per licence per month yield 80, 100 and 120; no leg's amount is ever summed into another leg's debt). A grant authorizes the exact purchase terms — pinned price binding or accepted price rule, units, scope, coverage and consumption policy — not a final usage amount. Legs whose metering requirements are explicitly compatible may share one measured basis as an optimization; legs with different metering or aggregation are counted once per their own authorized grain, and a leg whose tariff cannot be rated from the service's usage type is not ready. Independent commitment periods are deferred. Initial automated chains admit only conditional, reversible preparations before the common activation trigger (Q5.1); financing is validated before activation and collected afterwards (Q12); active-chain terms must be compatible and no cancellation cascade exists (Q5.2). Requirements and examples are in the [commercial PRD](../../../../docs/arch/bss-commercial-model/PRD.md); mechanisms in the [commercial DESIGN](../../../../docs/arch/bss-commercial-model/DESIGN.md).

### Consequences

- Orders needs a commercial-buyer owner axis, an attachment order category, separately authorized cancellation of prepared attachment orders and a revised D-215 predicate.
- Subscriptions and Rating need chain-version-aware period facts and attribution segments, per-leg contribution grains and result identities, per-leg corrections and assignment evidence; existing single-payer, single-billing-group contracts do not deliver this, and the usage collector must supply interval evidence.
- The service occupancy key must stop depending on one payer when a subscription has several; funding limits move to Accounts.
- A supply gap (missing or ended upstream leg) denies readiness for the next period and invokes authorized remedies; it terminates nothing automatically and erases no obligation.
- Shared quantity changes are conditional preparations across legs without an atomic supplier commit; a timeout is an unknown outcome to reconcile, not a failure.
- Invoicing, Ledger and Payments must preserve each leg's debtor, issuer, payer, book and history; the Ledger owner must amend the hierarchy-derived payer rules.

### Confirmation

Before acceptance, each critical invariant in the [pre-acceptance scenario checks](../../../../docs/arch/bss-commercial-model/DESIGN.md#43-pre-acceptance-scenario-checks) needs an owner-agreed contract path: S1 sponsored purchases and occupancy, S2 three obligations from one quantity under replay, S3 prepared purchase with failed activation, S4 late usage attribution, payer transfer and replay, S5 commitment versus billing period combinations, S6 payer-only reads with fresh validation. Document analysis does not prove runtime behaviour; operational release additionally requires implementation and integration evidence.

## Pros and Cons of the Options

### A. One service subscription with seller-owned commercial legs and buyer-owned purchase orders

- Good, because one service lifecycle and one quantity match the delivered product.
- Good, because purchase authority and private commercial terms have explicit single owners.
- Bad, because per-leg billing, authority and recovery require new cross-component contracts.
- Bad, because the common-period restriction excludes otherwise valid supplier combinations.

### B. Independent subscription lifecycles for every commercial tier

- Good, because independent commercial periods can be represented directly.
- Bad, because lifecycle coupling and duplicate delivery or metering prevention need additional coordination; deferred.

### C. Resource-owned legs, or an unowned aggregate with custom party-axis authorization

- Good, because fewer owner boundaries are crossed on the service side.
- Bad, because upstream readers cross the resource owner's boundary, and custom party axes need platform support that does not exist and still leave field disclosure unsolved.

## More Information

The proposed component contracts are now specified in [commercial DESIGN §§3.8–3.11](../../../../docs/arch/bss-commercial-model/DESIGN.md#38-orders-terms-and-subscriptions-contracts) and [Accounts DESIGN §§3.7–4](../../accounts/docs/DESIGN.md#37-database-schemas--tables), including GTS schemas, extension/validation/storage rules, SDK operations, state transitions, transaction boundaries, recovery and financial revision handling. These close the design choices for review; owner acceptance and runtime verification remain separate, and this ADR stays **proposed**.

Industry analogues support independent sales and restricted price views; they are not evidence about this platform's authorization model or supplier integration. The Q labels keep discussion traceability; they do not mean approval.

## Traceability

- [Commercial PRD](../../../../docs/arch/bss-commercial-model/PRD.md): `cpt-cf-bss-commercial-model-fr-purchase-authority`, `cpt-cf-bss-commercial-model-fr-chain-topology`, `cpt-cf-bss-commercial-model-fr-conditional-purchase`, `cpt-cf-bss-commercial-model-fr-independent-finance`, `cpt-cf-bss-commercial-model-nfr-replay`
- [Commercial DESIGN](../../../../docs/arch/bss-commercial-model/DESIGN.md): `cpt-cf-bss-commercial-model-design-architecture`
- [Ownership ADR](0002-cpt-cf-bss-adr-commercial-ownership-tenant-rbac.md): `cpt-cf-bss-adr-commercial-ownership-tenant-rbac`
- [Accounts PRD](../../accounts/docs/PRD.md): `cpt-cf-bss-accounts-fr-use-grant`
