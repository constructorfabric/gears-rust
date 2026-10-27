---
description: "Decision separating subscription commercial boundaries from Rating monetary calculation and Billing accounting."
status: proposed
date: 2026-07-15
decision-makers: "BSS Subscriptions team"
---

Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-08-24 by Virtuozzo International GmbH

# ADR-0002: Subscriptions Owns the Change Boundary; Rating Owns the Math

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Subscriptions computes proration (self-contained)](#subscriptions-computes-proration-self-contained)
  - [Subscriptions owns only the boundary; rating owns the math (chosen)](#subscriptions-owns-only-the-boundary-rating-owns-the-math-chosen)
  - [A third proration service](#a-third-proration-service)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-subscriptions-adr-when-not-math-split`

## Context and Problem Statement

A supported component addition or quantity increase changes the commercial composition at one applied instant. Financial consumers need both that boundary and the frozen accepted terms to reproduce the resulting charge. If Subscriptions and Rating each calculate money or independently infer the boundary, retries and corrections can produce different obligations.

## Decision Drivers

- One authoritative commercial boundary for all downstream consumers.
- Deterministic valuation from frozen accepted terms and truthful served intervals.
- No monetary calculation in Subscriptions.
- Immutable posted invoices and recoverable corrections owned by Billing.

## Considered Options

1. Calculate proration and tariff values in Subscriptions.
2. Publish commercial boundaries and amount-less facts; let Rating calculate tariff values.
3. Add a separate shared proration service.

## Decision Outcome

Subscriptions owns the actual commercial change boundary, composition and served/posture intervals. Pricing owns accepted prices and tariff rules. Subscriptions freezes the accepted bindings and publishes versioned amount-less facts with complete temporal slices and purchase-lot allocations. Rating values those facts, including recurring proration, supported minima, usage windows and separately agreed one-time actions. Billing owns invoice accounting/posting, allocated-prepayment application, final invoice rounding and posted corrections. Taxes and payment execution retain their respective owners.

For an immediate quantity increase at T, the same obligation's successor fact describes the prior quantity over `[periodStart, T)` and the complete accepted lot allocation over `[T, periodEnd)`. Existing lots retain their accepted terms. A delta purchase does not reprice them. Subscriptions does not calculate a day-count amount, aggregate usage or resolve monetary overrides. Differing-binding increases remain gated until the versioned lot-aware consumer contract is available.

An independently agreed chargeable action has one durable occurrence identity. A lifecycle transition, transport retry, price change or rebuilt attempt cannot create a fee by itself. The fact retains qualifying-action evidence and the accepted flat or unit basis. Rating determines its value.

Every commercial replacement retains the original economic identity and publishes a successor seal naming exact fact versions and digests. Billing accepts the complete required Rating results for that seal and applies a draft replacement or supported posted correction. Publication alone proves neither payment nor refund completion.

### Consequences

- One boundary and one tariff evaluator govern replay.
- Subscriptions retains commercial evidence without taking over monetary calculation or raw-usage storage.
- Rating calculation revisions and Billing financial targets remain separate from commercial fact and seal versions.
- Owner SDK contracts, supported valuation profiles and executed S-U08/S-U13 fixtures gate use of the capability.

### Confirmation

- A mid-cycle increase preserves original terms and quantities before T and the full lot allocation afterward.
- Rating computes the expected adjustment from frozen inputs; Subscriptions contains no independent proration evaluator.
- Duplicate or reordered facts cannot create another obligation or override a newer head.
- A posted correction retains original invoice history and recovers uncertain financial execution by the same identity.

## Pros and Cons of the Options

### Subscriptions computes proration (self-contained)

- Keeps one call path for lifecycle and arithmetic.
- Duplicates Rating's tariff engine and creates two authorities for monetary replay.

### Subscriptions owns only the boundary; rating owns the math (chosen)

- Keeps commercial and financial authority explicit and allows independent deterministic replay.
- Requires complete immutable input, replacement and recovery contracts across owners.

### A third proration service

- Centralizes a shared calculation.
- Adds another contract for work already owned by Rating, without simplifying commercial evidence or Billing corrections.

## More Information

[Composition changes](../features/03-changes.md), [commercial facts](../features/08-commercial-facts.md), [Rating contract](../DESIGN.md#rating-result-contract) and [financial consumer boundary](../DESIGN.md#financial-consumer-boundary) define the boundary and recovery requirements.

## Traceability

- **PRD:** [PRD.md](../PRD.md), `fr-proration-ownership`, `fr-proration-triggers`, `fr-update-quantity`, `fr-versioned-billable-facts`, `fr-one-time-occurrence`.
- **Design:** [DESIGN.md](../DESIGN.md).
