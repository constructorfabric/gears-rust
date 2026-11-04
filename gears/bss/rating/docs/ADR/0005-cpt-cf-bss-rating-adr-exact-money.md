---
status: proposed
date: 2026-10-12
decision-makers: "BSS Rating team"
---

Created:  2026-10-12 by Virtuozzo International GmbH
Updated:  2026-10-12 by Virtuozzo International GmbH

# ADR-0005: Exact Major-Unit Money per the BSS Money ADR

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Exact fractions in major units (chosen)](#exact-fractions-in-major-units-chosen)
  - [Exact fractions in minor units](#exact-fractions-in-minor-units)
  - [rust_decimal everywhere](#rust_decimal-everywhere)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-rating-adr-exact-money`

## Context and Problem Statement

Rating multiplies prices by quantities and proration fractions. A product of two 28-digit decimals,
or a fraction such as `1/3` of a period, has no exact `rust_decimal::Decimal` form. The earlier
design (T-D-51 as written on 2026-10-01) kept exact fractions in **minor** units: Rating converted
Pricing's major-unit decimals as `decimal x 10^currency_scale`.

The cross-BSS [money ADR](../../../docs/ADR/0001-cpt-cf-bss-adr-exact-decimal-boundaries.md)
(BSS ADR-0001, exact decimal boundaries) was merged on `main` on 2026-10-11 at `510ce99f0`; its
front matter still says `status: proposed`. It sets one rule set for every BSS gear: money in major
units, exact fractions for intermediate amounts, canonical text on every wire, published limits,
declared business rounding only, and line rounding at the invoice-line owner.

Which numeric form does Rating use for amounts in computation, storage and contracts, and who
rounds?

## Decision Drivers

* A stored result must re-execute to the same amount years later; no step may lose a digit
  (money ADR rule 4).
* Pricing sends money as exact major-unit decimals; Billing posts major-unit `PostedMoney`. A
  minor-unit form inside Rating adds two conversions and contradicts rule 2.
* Every producer publishes limits and every consumer proves its own (rule 3).
* No Rating V1 contract is implemented, so the V1 draft can change without a V2 (T-D-71).

## Considered Options

1. Exact reduced fractions in major units (chosen).
2. Exact reduced fractions in minor units (the earlier design).
3. `rust_decimal::Decimal` everywhere.

## Decision Outcome

Chosen option: **exact fractions in major units, adopting the money ADR** (DECISIONS T-D-80, which
amends T-D-51, T-D-66 and T-D-74).

- **Form.** Every amount is a reduced fraction in major units with a positive denominator, the sign
  on the numerator and zero as `0/1`, carrying `currency` and `currency_scale`. Rating's wire DTO is
  `ExactMoney {numerator, denominator, currency, currency_scale}`. A field declared as a fraction
  stays one when its value is an integer. Quantities are `ExactQuantity` finite decimals with their
  unit and no currency.
- **Arithmetic.** Rating computes with the shared `bss_money::exact::ExactAmount` (crate
  `cf-gears-bss-money`, [`gears/bss/libs/money`](../../../libs/money/src/exact.rs)). A Pricing
  decimal enters as the reduced fraction of its digits over `10^scale`. `rust_decimal::Decimal` is
  used only where the value provably fits (Pricing money as received; the Products evaluator per
  granule; a granule whose `Add`, `Sub`, `Mul` or `DivConst` result would leave `Decimal`'s exact
  range fails
  `derived_formula_inexact`, because Products' evaluator would round it). Floating point is never used.
- **Scale.** `currency_scale` comes from the binding of record: the bound price's stored currency
  spec, `price.currency.scale()` (pricing D-531; `InvoiceInputs` carries no scale since `958b5e2f1`), and
  is the posting increment, never a multiplier or a limit on a rate's digits (`0.047` EUR is a valid
  rate at scale 2). The scale is pinned per binding of record; a later resolve with another scale is
  a new binding whose slices form their own lines, because the scale is part of `line_key` and
  `invoice_line_key`; nothing is converted, and mixed currencies fail `binding_currency_mismatch`.
- **Line rounding is Billing's.** HALF_EVEN is the invoice-line owner's rule (money ADR rule 7), so
  no Rating contract carries a rounding mode. A minimum-fee top-up carries the invoice line key of
  the one rated line it tops up, so Billing rounds both together (rule 7 step 2); a floor across
  several lines is Billing's rule (R-32).
- **Canonical text.** Numerators, denominators, quantities and decimals are canonical strings on
  every wire; `currency_scale` is a JSON integer. A JSON number in such a field is
  `InvalidArgument` (quarantined or `usage_malformed` in a consumed event or feed entry). The same
  text feeds every Rating digest over numbers.
- **Bounds.** After reduction `abs(numerator) < 10^84` and `1 <= denominator < 10^64`;
  `ExactQuantity` has at most 60 integer and 28 fractional digits. DESIGN §3.3 derives these from
  the producers' published limits. Beyond them evaluation fails `precision_overflow` or
  `quantity_out_of_range` before persistence or delivery. Storage is PostgreSQL `NUMERIC(84,0)`,
  `NUMERIC(64,0)` and `NUMERIC(88,28)` beside `currency` and `currency_scale`; other profiles use
  canonical text columns.
- **Rounding.** Rating applies only declared business rounding: Products' formula and output
  rounding inside `derived::evaluate`, and the package block `ceil`. Rating never rounds money and
  never reads a posted amount; it does not call the crate's posting functions
  (`round_half_even`, `into_posted_exact`). Order evaluation adds a display value rounded
  `HALF_EVEN`, marked as a non-authoritative estimate, beside the exact amount (T-D-79).
- **Billing.** Billing sums contributions per `invoice_line_key`, rounds each invoice-line total
  once, `HALF_EVEN`, at the stored `currency_scale` to `PostedMoney`, and posts a correction as the
  rounded corrected total minus the cumulative posted amount (rules 7 and 8; ADR-0004).

### Consequences

* No Rating contract carries integer minor units or nano-minor values.
* Rating and Billing share one arithmetic crate, so their exact sums agree by construction.
* The bounds are Rating's proposal until Billing accepts the delivery bounds and producers publish
  narrower limits (R-32, R-33). A change of the bounds is a new engine generation (ADR-0006).
* Pricing's `amount_for`, which computes in `Decimal`, is a test oracle only on cases where every
  intermediate of `amount_for` is exact in `Decimal`, not merely its result.

### Confirmation

* Golden vectors: a `1/3` proration, a `0.047` EUR rate at scale 2, and a line of `1/200` EUR
  delivered as `{"numerator": "1", "denominator": "200", "currency": "EUR", "currency_scale": 2}`.
* Boundary tests at `10^84 - 1` and `10^84` for the numerator and at `10^64` for the denominator.
* Wire tests reject a JSON number in every exact field.
* A storage round trip on PostgreSQL and SQLite preserves the canonical text byte for byte.

## Pros and Cons of the Options

### Exact fractions in major units (chosen)

* Good: one unit from Pricing to Billing; no conversion; matches the cross-BSS contract; exact for
  every product and proration.
* Bad: big-integer arithmetic and wider storage columns; the bounds need a written proof.

### Exact fractions in minor units

* Good: the same exactness as option 1.
* Bad: two conversions (`x 10^currency_scale` in, divide out at posting) that every consumer must
  repeat; a minor-unit field on a contract contradicts money ADR rule 2; an error in the scale
  silently moves an amount by a factor of 100.

### rust_decimal everywhere

* Good: one familiar type with fixed-size storage.
* Bad: not exact. It keeps 28 significant digits, and its checked operations round a result beyond
  28 fractional digits instead of failing: `checked_mul(1e-28, 1e-28)` returns `Some(0)` in
  rust_decimal 1.41, the version the workspace resolves. A checked operation therefore proves
  nothing about exactness, and `1/3` has no `Decimal` form at all.

## More Information

- Money ADR rules 1 to 9, Consequences and Adoption:
  [`../../../docs/ADR/0001-cpt-cf-bss-adr-exact-decimal-boundaries.md`](../../../docs/ADR/0001-cpt-cf-bss-adr-exact-decimal-boundaries.md).
- The "Exact numbers" table and the bound derivation:
  [DESIGN §3.3](../DESIGN.md#33-api-contracts).

## Traceability

- **Decisions**: [`../DECISIONS.md`](../DECISIONS.md) T-D-80 (this decision), T-D-51, T-D-66,
  T-D-74 (amended), T-D-79, R-14, R-32, R-33.
- **Adopts**: BSS ADR-0001 (exact decimal boundaries).
- **Design**: [DESIGN §3.3](../DESIGN.md#33-api-contracts), [DESIGN §3.7](../DESIGN.md#37-database-schemas--tables).
- **Related ADRs**: [ADR-0004](./0004-cpt-cf-bss-rating-adr-absolute-revisions.md),
  [ADR-0006](./0006-cpt-cf-bss-rating-adr-engine-generations.md).
