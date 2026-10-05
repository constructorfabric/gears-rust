# ADR-0001: One Price Book per Currency

**ID**: `cpt-cf-bss-pricing-adr-book-per-currency`

- [ ] `p1` - **ADR implementation status**

**Status:** accepted 2026-09-25 · **Deciders:** product owner · **Source:** spec §2 decisions 4–5 and 16; §5; §8.

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
- [More Information](#more-information)

<!-- /toc -->

## Context and Problem Statement

The former charge-line and market-price keys mix commercial identity with currency, region, cohort and plan structure. Operators need to see the same book-wide money for every plan that reads it.

## Decision Drivers

`cpt-cf-bss-pricing-fr-price-book`, `cpt-cf-bss-pricing-fr-entry-key`, `cpt-cf-bss-pricing-fr-book-export`. The approved PriceBook model is the content authority. The decision must hold under tenant isolation,
concurrent writes and either supported database, and must remain explainable to operators and reviewers.

## Considered Options

1. Keep the eight-axis charge line and market-price split.
2. Use per-currency books.
3. Use a single multi-currency book.

## Decision Outcome

Chosen: **Use per-currency books**. One currency per book; one entry per SKU × charge kind × period in that book. Region becomes an optional dimension value, and variant disappears. A plan revision binds one book; plan-specific money requires another book or SKU.

### Consequences

Currency and validity are inspectable at the book boundary. The entry key has no plan or variant. A plan-specific exception costs a new book or SKU; that duplication is intentional.

### Confirmation

Tests enforce tenant book-code uniqueness, normalized nullable-period entry-key uniqueness and revision book membership. Export exposes only the chosen book currency. A foreign-book item returns ITEM_BOOK_FOREIGN in phase 3. These are implementation acceptance conditions; Part 2a replaces documents only.

## Pros and Cons of the Options

The legacy split preserves compatibility but retains redundant axes. Per-currency books simplify operator reasoning at the cost of duplicate books for plan-specific money. A multi-currency book hides the commercial boundary and retains currency branching in each price.

## More Information

[PRD](../PRD.md), [DESIGN](../DESIGN.md), [DECISIONS](../DECISIONS.md).
Source: `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout, §2 decisions 4–5 and 16; §5; §8.
The superseded set remains on `bss/products-backup` and in history.
