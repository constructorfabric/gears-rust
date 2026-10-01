---
status: superseded
date: 2026-07-10
decision-makers: "BSS Rating team"
---

Created:  2026-08-24 by Virtuozzo International GmbH
Updated:  2026-10-08 by Virtuozzo International GmbH

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
  - [Adopt the pricing canonical key verbatim (chosen)](#adopt-the-pricing-canonical-key-verbatim-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-rating-adr-scope-key-adoption`

> **Superseded (2026-10-08) by [ADR-0003](./0003-cpt-cf-bss-rating-adr-pricebook-bindings.md) and DECISIONS T-D-73.**
> Pricing on `main` is the PriceBook model: price books, entries per SKU, dated prices per
> dimension-value chain, plans and plan revisions. The scope key, `PriceWindow`, `PriceOverlay`,
> phases, cohorts, eligibility classes and the catalog-version contract this ADR adopted no longer
> exist (pricing D-384…D-397; DECISIONS T-D-37). Pricing selects the price — `resolve` binds the
> price in force for a signup and walks a pinned chain for a renewal (pricing D-420) — and Rating
> prices the binding it is given. The text below is the record of the pre-PriceBook decision and is
> not to be implemented.

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
3. Adopt the pricing canonical key verbatim (chosen).

## Decision Outcome

Chosen option: **Adopt the pricing canonical scope key verbatim** — for both selection and the
non-overlap invariant — ten axes since pricing D-196 (2026-08-06; eight at this ADR's decision
date): `(planId, currency, region, priceOverlay, phase, priceEligibility, chargeKind, cohort)` plus
the usage pair `(skuId, dimensionKey)` (`none` on non-usage rows; row identity `(skuId,
dimensionKey)` since pricing D-372, T-D-35); the original decision text named the first eight. `phase` is a `phase_id` (uuid). Within
`existing_grandfathered`, the generation is selected by the `cohort` of the subscription's **pinned
price id** (originally read from `pricingSnapshotRef`; since 2026-09-25 from the subscription version's pinned price ids — see the note below), never by `activatedAt` alone. Eligibility classes order
`existing_grandfathered > new_subscriptions_only > all_subscriptions`.

> **Amendment (2026-08-25).** Pricing D-196 (2026-08-06) widened the canonical key it owns to
> **ten axes**: the usage pair `(meter, dimensionKey)` joined as normative axes (`none` on
> non-usage rows; the pricing store holds one price row per usage line, both unique indexes key
> over the pair). This ADR's adoption is *verbatim*, so the adopted key is the full ten; the
> 8-axis spelling above is the key as of the decision date. The slice-02 `SelectionKey`
> carries the full ten since T-D-35 (2026-08-25; SEAMS K6 resolved).

**Amendment (2026-09-25)**: the decision is unchanged in substance — adopt the owner's key, whatever
its axis count — and the axis list is updated to the ten axes pricing implements. `meter` and
`dimension_key` make usage rows of one plan distinct per meter/dimension, which Rating already
selects on in step 3. Pricing enforced window non-overlap per `price_id` in the database
(`pricing/src/domain/scope_key.rs`, removed with the PriceBook model); Rating's selection guard
fails closed on more than one match on the full key.

**Challenge recorded (2026-10-01)**: the Seam Atlas v2 baseline (D02/D06) moves price selection to
order acceptance in a PriceBook Pricing model and has Rating load bindings by price id without
re-resolving. Rating additionally asserts that its selection equals the `priceId` the commercial
fact carries (`binding_mismatch`). The cohort pin moved from the snapshot pre-stamp to the
subscription version's pinned price ids (slice 02).

**Superseded by T-D-37 (decided 2026-09-25)**: the PriceBook model is now this repository's
Pricing. The canonical scope key, phases, cohort and the catalog-version contract this ADR adopts
are superseded; Rating adapts against `PricingReadV1::resolve` (`GET /bss-pricing/v1/resolve`) and
`GET /bss-pricing/v1/prices/{id}`, which return the bound price per chain, so price-binding
authority (formerly R-18) is decided. The selection rule above is the record of the pre-PriceBook
design; the adapter is designed in ADR-0003 and DECISIONS T-D-73 (R-17 closed 2026-10-08).

### Consequences

* "At most one window matches" holds **on the full key**; coexisting `chargeKind` / `cohort` rows are disambiguated, not rejected.
* Tariffs carries no independent key definition and cannot drift from the SoR (guarded jointly with the pricing gear).
* Multi-generation grandfathering needs no new store — the pin already exists in the subscription version's pinned price ids (the accepted price binding; Subscriptions names the field `pricingSnapshotRef`, which is not the Rating snapshot — DECISIONS R-29).

### Confirmation

* A joint fixture: a hybrid plan (`recurring` + `usage`) and a grandfathered plan with ≥ 2 `cohort` generations both resolve to exactly one row per line without a non-overlap failure.
* The Rating step-2 selection key and the pricing gear's `pricing_price` uniqueness key (`uq_pricing_price_scope_key_*`, ten columns) were identical in the shared fixture set (pre-PriceBook; that table, its keys and the fixture corpus were removed with the PriceBook model — superseded by T-D-37 and ADR-0003, T-D-73).

## Pros and Cons of the Options

### Keep the 4-axis key with post-selection filters

* Good: minimal change to the incoming PRD.
* Bad: non-unique selection on hybrid and multi-generation catalogs; fails-closed on legitimate rows; grandfathering by `activatedAt` cannot disambiguate coexisting generations.

### Define a Tariffs-local key

* Good: self-contained.
* Bad: guaranteed drift from the pricing SoR; contradicts the cross-team contract in pricing `ADR/0001`/`ADR/0002`; duplicates an identity the catalog already owns.

### Adopt the pricing canonical key verbatim (chosen)

* Good: unique selection; no drift; no new store; honors the ratified cross-gear contract.
* Bad: Tariffs must consume `priceEligibility`, `chargeKind`, and `cohort` from the pinned price row — a bounded contract addition, already available via the pinned price id.

## More Information

Cross-gear seam analysis (seams K1-K5 and K6 (K6 resolved 2026-08-25, T-D-35)), rationale, and the ownership matrix were in
the former `SEAMS.md`; the adopted seam keys are now in [DESIGN §3.5](../DESIGN.md#adopted-pricing-semantics) and the
dependency evidence in [`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.4 (2026-10-06 documentation restructure; the decision is unchanged). Pricing-side ADRs: Canonical scope key (superseded by the PriceBook model, see T-D-37 in rating DECISIONS),
Grandfathering cohort axis (superseded by the PriceBook model, see T-D-37 in rating DECISIONS).

## Traceability

- **PRD**: [`../PRD.md`](../PRD.md) §1.4 (`PriceWindow`, `Price eligibility`), §6.3 (step 2), §6.5.
- **Seams**: adopted seam keys K1-K5, K6 — [DESIGN §3.5](../DESIGN.md#adopted-pricing-semantics); pricing evidence — [`../UPSTREAM_REQS.md`](../UPSTREAM_REQS.md) §2.4.
- **Design**: [DESIGN contract 02](../DESIGN.md#contract-02) (former `design/02-selection-eligibility.md`); implemented by [`../features/02-evaluation-core.md`](../features/02-evaluation-core.md).
- **Superseded by**: [ADR-0003](./0003-cpt-cf-bss-rating-adr-pricebook-bindings.md) (`cpt-cf-bss-rating-adr-pricebook-bindings`); DECISIONS T-D-37, T-D-73.
