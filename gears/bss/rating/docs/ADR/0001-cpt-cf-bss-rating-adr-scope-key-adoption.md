---
status: accepted
date: 2026-07-10
decision-makers: "BSS Rating team"
---

Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-01 by Virtuozzo International GmbH

# ADR-0001: Adopt the Pricing Canonical Scope Key (Do Not Define a Tariffs Key)

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Keep the 4-axis key with post-selection filters](#keep-the-4-axis-key-with-post-selection-filters)
  - [Define a Tariffs-local key](#define-a-tariffs-local-key)
  - [Adopt the pricing 8-axis canonical key (chosen)](#adopt-the-pricing-8-axis-canonical-key-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-rating-adr-scope-key-adoption`

## Context and Problem Statement

The Tariffs PRD originally selected `Price`/`PriceWindow` at step 2 on a 4-axis tuple
`(planId, currency, region, phase)` and asserted "at most one window matches". The pricing gear is
the ratified System of Record for the scope key and **deliberately publishes many concurrently
active rows** on that shorter tuple: a hybrid plan holds `recurring` **and** `usage` rows (differing
only in `chargeKind`), and grandfathering keeps N `cohort` generations live alongside the successor
— all active at one `t` on one `(planId, currency, region, phase)`.

Under the 4-axis selection those are multiple matches; "at most one window matches" therefore
**fails-closed on exactly the legitimate catalogs the pricing gear engineered**. What key must
Tariffs use so that selection is unique without banning hybrid / grandfathered / phased plans?

## Decision Drivers

* Selection and non-overlap must key off **one** identity, used identically by the catalog and by Tariffs — divergence re-introduces the collisions the pricing ADRs removed.
* Hybrid `chargeKind` rows and multiple grandfathering `cohort` generations must be **distinct** identities, disambiguated rather than rejected.
* Tariffs is an evaluation consumer, not the scope-key SoR; the pricing gear owns the key (`ADR/0001`, `ADR/0002`) and binds Tariffs to it as a cross-team contract.
* No new store: generation selection must ride an input Tariffs already has.

## Considered Options

1. Keep the 4-axis key with post-selection `priceEligibility` filters.
2. Define a Tariffs-local key.
3. Adopt the pricing 8-axis canonical key verbatim (chosen).

## Decision Outcome

Chosen option: **Adopt the pricing canonical scope key verbatim** — for both selection and the
non-overlap invariant. As implemented by pricing (`pricing/src/domain/scope_key.rs`, pricing D-196)
the key has **ten** axes: `(plan_id, currency, region, price_overlay, phase, price_eligibility,
charge_kind, cohort, meter, dimension_key)`; the original decision text named the first eight. `phase` is a `phase_id` (uuid). Within
`existing_grandfathered`, the generation is selected by the `cohort` of the subscription's **pinned
price id** (originally read from `pricingSnapshotRef`; since 2026-09-25 from the subscription version's pinned price ids — see the note below), never by `activatedAt` alone. Eligibility classes order
`existing_grandfathered > new_subscriptions_only > all_subscriptions`.

**Amendment (2026-09-25)**: the decision is unchanged in substance — adopt the owner's key, whatever
its axis count — and the axis list is updated to the ten axes pricing implements. `meter` and
`dimension_key` make usage rows of one plan distinct per meter/dimension, which Rating already
selects on in step 3. Pricing enforces window non-overlap per `price_id` in the database; Rating's
selection guard fails closed on more than one match on the full key.

**Challenge recorded (2026-10-01, decision unchanged)**: the Seam Atlas v2 baseline (D02/D06) moves
price selection to order acceptance in a PriceBook Pricing model and has Rating load bindings by
price id without re-resolving. That Pricing model does not exist in this repository (DECISIONS
R-17). Until the owners decide R-18, this ADR stands and Rating additionally asserts that its
selection equals the `priceId` the commercial fact carries (`binding_mismatch`). The cohort pin
moved from the snapshot pre-stamp to the subscription version's pinned price ids (slice 02).

### Consequences

* "At most one window matches" holds **on the full key**; coexisting `chargeKind` / `cohort` rows are disambiguated, not rejected.
* Tariffs carries no independent key definition and cannot drift from the SoR (guarded jointly with the pricing gear).
* Multi-generation grandfathering needs no new store — the pin already exists in `pricingSnapshotRef`.

### Confirmation

* A joint fixture: a hybrid plan (`recurring` + `usage`) and a grandfathered plan with ≥ 2 `cohort` generations both resolve to exactly one row per line without a non-overlap failure.
* The Rating step-2 selection key and the pricing gear's `pricing_price` uniqueness key (`uq_pricing_price_scope_key_*`, ten columns) are identical in the shared fixture set.

## Pros and Cons of the Options

### Keep the 4-axis key with post-selection filters

* Good: minimal change to the incoming PRD.
* Bad: non-unique selection on hybrid and multi-generation catalogs; fails-closed on legitimate rows; grandfathering by `activatedAt` cannot disambiguate coexisting generations.

### Define a Tariffs-local key

* Good: self-contained.
* Bad: guaranteed drift from the pricing SoR; contradicts the cross-team contract in pricing `ADR/0001`/`ADR/0002`; duplicates an identity the catalog already owns.

### Adopt the pricing 8-axis canonical key (chosen)

* Good: unique selection; no drift; no new store; honors the ratified cross-gear contract.
* Bad: Tariffs must consume `priceEligibility`, `chargeKind`, and `cohort` from the frozen snapshot — a bounded contract addition, already available via the pinned price id.

## More Information

Cross-gear seam analysis (seams K1-K5), rationale, and the ownership matrix are in
[`../SEAMS.md`](../SEAMS.md). Pricing-side ADRs: `cpt-cf-bss-pricing-adr-canonical-scope-key`,
`cpt-cf-bss-pricing-adr-grandfathering-cohort-axis`.

## Traceability

- **PRD**: [`../PRD.md`](../PRD.md) §1.4 (`PriceWindow`, `Price eligibility`), §6.3 (step 2), §6.5.
- **Seams**: [`../SEAMS.md`](../SEAMS.md) K1-K5.
- **Design**: [`../design/02-selection-eligibility.md`](../design/02-selection-eligibility.md).
