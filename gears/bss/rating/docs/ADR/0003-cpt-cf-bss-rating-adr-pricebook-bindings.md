---
status: proposed
date: 2026-10-08
decision-makers: "BSS Rating team"
---

Created:  2026-10-08 by Virtuozzo International GmbH
Updated:  2026-10-08 by Virtuozzo International GmbH

# ADR-0003: Rate the Bindings Pricing Resolves

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Keep Rating-side selection over catalog documents](#keep-rating-side-selection-over-catalog-documents)
  - [Call resolve on every evaluation and keep no binding](#call-resolve-on-every-evaluation-and-keep-no-binding)
  - [Resolve with the subscription's pins and keep the binding of record (chosen)](#resolve-with-the-subscriptions-pins-and-keep-the-binding-of-record-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-rating-adr-pricebook-bindings`

## Context and Problem Statement

[ADR-0001](./0001-cpt-cf-bss-rating-adr-scope-key-adoption.md) had Rating select a price row
itself. It used the pricing scope key over plan and overlay documents at a pinned catalog version.
That model no longer exists. On `main`, Pricing is the **PriceBook** model (pricing D-384…D-397;
DECISIONS T-D-37):

- a price book has entries per SKU;
- an entry holds dated prices per dimension-value chain;
- plans have revisions;
- there is no scope key, `PriceWindow`, `PriceOverlay`, phase, cohort or catalog version.

Pricing exposes the read contract `PricingReadV1::{resolve, price, current_revision}`
(`GET /bss-pricing/v1/resolve`, `GET /bss-pricing/v1/prices/{id}`; pricing D-419…D-425):

- **`resolve`** answers one plan revision on one date with the whole chain matrix per item.
  - Each cell is a binding or *uncovered*.
  - A signup binds the price in force.
  - A renewal walks the pinned chain through `all` successors and stops before the first `new`
    one (D-420).
- **Pins** are the consumer's. Pricing stores none, and freezes no descriptors on a price (D-389).
- **`price(price_id)`** serves an approved price's money forever (D-422).

How does Rating obtain the price it rates with, and keep every result reproducible later?

## Decision Drivers

* Pricing owns price selection. Rating must not keep a second selection model that can drift from it (ADR-0001's own principle: adopt the owner's rule, don't fork it).
* A result must replay byte-identically years later from Rating's own store (DECISIONS T-D-41), although Pricing keeps no binding and re-reads SKU descriptors as of each date (D-421).
* An ordinary correction (late or withdrawn usage) must never move a window onto a different price; only an administrative re-rate may (T-D-21, T-D-24).
* The subscription's price state across renewals belongs to Subscriptions: it holds the pins per period (SUB-D-29), and the first period's pins are the accepted order's bindings (Orders D-162).
* A price can end inside a period; Pricing names that point `ends_on`, the only slice point (D-425).

## Considered Options

1. Keep Rating-side selection over catalog documents.
2. Call `resolve` on every evaluation and keep no binding.
3. Resolve with the subscription's pins at the period start and at each `ends_on`, store the binding of record verbatim, and reuse it for corrections (chosen).

## Decision Outcome

Chosen option: **resolve with the subscription's pins and keep the binding of record** (DECISIONS
T-D-73).

- **Resolve call.** For a commercial fact, Rating calls `resolve(revision_id, date = period start,
  pins)`. The pins are the period's pins that the fact carries from Subscriptions (SUB-D-29).
  - At a binding's `ends_on` inside the period, Rating resolves again on that date with the same
    pins for the rest of the period (D-397, D-425). `effective_to` is never a slice point.
  - An uncovered cell fails closed (`price_uncovered`) and is never priced at zero.
  - Rating calls as the system subject `bss-rating.system`, with pricing `plan:read` for resolve
    and `price:read` for the pinned read (D-424).
- **Binding of record.** The binding of record is stored verbatim, in insert-only
  `bss_rating__pricing_binding`. It holds:
  - `(revision_id, date, pins)`;
  - the returned `AcceptedBinding`s, with their money, SKU version, usage rating policy, meter and
    invoice inputs.

  It replaces the pin of record. Corrections reuse it and never resolve again.
- **Replay.** Replay reads the stored binding and may verify `price(price_id).money_digest`, which
  is served forever for an approved price (D-422). A cancelled price is served with its cancelled
  state (D-520).
- **Re-binding.** A final window resolves again with the fact's current pins only when an
  administrative re-rate asks for it (`rebind = true`) or a new fact version changes its pricing
  query (plan revision, period or pins). Every other correction reuses the binding of record.
- **What Rating no longer does.** Rating does no selection, so it uses no scope key, cohort, phase,
  eligibility class or overlay. It keeps one check: `binding_mismatch`, which compares the
  accepted binding the fact carries with the resolved one.

### Consequences

* Rating's evaluation input is the resolved bindings (`PricingInput`), not plan documents. Money arrives as exact major-unit decimals and stays in major units as exact fractions carrying `currency` and `currency_scale`, without rounding (T-D-74, amended 2026-10-09 by T-D-80 to follow the proposed cross-BSS money ADR).
* Each usage binding carries its immutable `UsageRatingPolicy`, which sets the window and aggregation (T-D-75). Its `meter` names a Products derived usage type (T-D-76).
* Rating stores what Pricing does not keep: the binding, the descriptors and the derived declaration.
* Live rating depends on Subscriptions carrying the period's pins, plan revision and `BillingTerms` on each fact. That contract is not built yet (DECISIONS R-03; UPSTREAM_REQS §2.6).
* ADR-0001 is superseded.

### Confirmation

* Adapter tests read pricing's frozen golden consumer contracts (`gears/bss/pricing/pricing/tests/contract/*.json`). Covered cases:
  - signup;
  - renewal walk: pin 10, then `all` 12, then `new` 15, binds 12;
  - a moved default pin;
  - an ended chain;
  - invoice inputs;
  - refusals.
* A replay of a stored binding produces the same outcome after the price chain has moved on.
* A correction after an administrative re-rate uses the re-rated binding.

## Pros and Cons of the Options

### Keep Rating-side selection over catalog documents

* Good: no change to the evaluation core's selection step.
* Bad: impossible. The scope key, windows, overlays, cohorts and catalog versions it selects over were removed from Pricing (D-384…D-397), and pricing refuses a legacy schema.

### Call resolve on every evaluation and keep no binding

* Good: no Rating-side copy of pricing data.
* Bad: replay and corrections are not reproducible. Pricing freezes no descriptors on a price (D-389), resolve re-reads SKU versions as of the date (D-421), and a renewal walk moves with new successors. A correction could then land on a different price than the original result.

### Resolve with the subscription's pins and keep the binding of record (chosen)

* Good:
  - Pricing stays the only selector.
  - Results replay from Rating's store.
  - Corrections keep their price.
  - Renewal semantics are Pricing's and Subscriptions'.
* Bad: Rating depends on Subscriptions delivering the period's pins, which is not built (SUB-D-29 defers its adapter). Rating also keeps a verbatim binding copy per resolve, which costs storage.

## More Information

- **Pricing decisions:**
  - D-389: prices freeze no descriptors.
  - D-397: consumer pins replace cohorts and catalog versions.
  - D-419, D-420, D-421: the resolve contract and walk rules.
  - D-422: price by id.
  - D-424: grants.
  - D-425: `ends_on`.
- **Subscriptions:** SUB-D-29 (the PriceBook adapter; pins per period).
- **Orders Lifecycle:** D-162 (activation re-resolves with the accepted bindings as pins and stores them as the first period's pins).
- **Rating:**
  - DECISIONS T-D-37, and T-D-73…T-D-79;
  - DESIGN §3.5 (dependencies) and §4.1 (what is frozen);
  - UPSTREAM_REQS §2.4 (Pricing evidence).

## Traceability

- **PRD**: [`../PRD.md`](../PRD.md) §1.4 (binding, binding of record), §6.1 (`fr-snapshot-carry`), §6.3 (`fr-base-catalog-selection`), §9.2 (`contract-pricing-readmodel`), §17.1.
- **Supersedes**: [ADR-0001](./0001-cpt-cf-bss-rating-adr-scope-key-adoption.md) (`cpt-cf-bss-rating-adr-scope-key-adoption`).
- **Design**: [DESIGN §3.5](../DESIGN.md#35-external-dependencies), [DESIGN §4.1](../DESIGN.md#41-versioning-and-historical-correctness), [DESIGN contract 02](../DESIGN.md#contract-02); implemented by [`../features/02-evaluation-core.md`](../features/02-evaluation-core.md).
- **Decisions**: [`../DECISIONS.md`](../DECISIONS.md) T-D-37, T-D-73.
