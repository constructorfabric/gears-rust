# ADR-0001: No Product Entity — the SKU Is the Catalog's Unit

**ID**: `cpt-cf-bss-products-adr-no-product-entity`

- [ ] `p1` - **ADR implementation status**

**Status:** accepted 2026-09-24 · **Deciders:** product owner · **Source:** spec §2 decisions 2, 12 and 17, §3 items 32–33, §4

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Keep Product as an optional grouping](#keep-product-as-an-optional-grouping)
  - [Remove Product, flat Category (chosen)](#remove-product-flat-category-chosen)
  - [Free-text tag](#free-text-tag)
- [More Information](#more-information)

<!-- /toc -->

## Context and Problem Statement

The previous registry kept Product → SKU: name uniqueness on the Product, a parent-child lifecycle cascade and a category tree. No consumer read the Product; every plan, price and rating step keyed on `sku_id`. Operators created a Product only to reach a SKU.

## Decision Drivers

- Operator convenience: one entity to author and publish.
- Every consumer keys on `sku_id`.
- Lists and reports group by one category level.

## Considered Options

1. Keep Product as an optional grouping.
2. Remove Product; move name uniqueness to the SKU; keep a flat Category.
3. Remove Product and Category; a free-text tag on the SKU.

## Decision Outcome

Option 2. A SKU is an independent tenant-scoped commercial definition with its own type, category and lifecycle.
`sku.name` and `sku.code` are each unique per tenant. Category is a flat list with `is_default`, with one
category per SKU. A `bundle` SKU has no composition in Products; it is never priced and cannot be a plan item.
A Pricing plan is sold as it through a `sold_as` reference, protected by the same reservation barrier as price
book entries and plan items (spec §2 decision 17).

### Consequences

- The lifecycle cascade, containment and taxonomy-tree modules leave the gear.
- Reports group by category, one level; a `parent_id` column can be added later without a rewrite.
- Bundle composition lives in the plan (Pricing), never in the registry.

### Confirmation

After implementation, confirm that `cargo nextest run -p cf-gears-bss-products` passes and its tests no longer
model a Product entity; `GET /bss-products/v1/skus` is the catalog's root list; and
`cfs list-ids | grep cpt-cf-bss-products` contains no identifier defining a Product entity. The retained
`bss-products` namespace and this ADR's identifier do not define such an entity. These are implementation
confirmation criteria, not claims that Phase 0 has replaced the existing code.

## Pros and Cons of the Options

### Keep Product as an optional grouping

Bad: two spellings of one thing; every screen and rule branches on "has a Product".

### Remove Product, flat Category (chosen)

Good: one publish per sellable thing. Bad: reports lose a hierarchy they never used.

### Free-text tag

Bad: typos multiply categories; no rename.

## More Information

PRD requirements [`fr-sku-define`](../PRD.md#fr-sku-define), [`fr-category-flat`](../PRD.md#fr-category-flat)
and [`fr-sku-bundle`](../PRD.md#fr-sku-bundle); spec §4; the prototype's SKU list (`~/Projects/diffora/ui-prototype/pricebook/`).
