---
status: proposed
date: 2026-10-08
decision-makers: "Owners of money, metering and invoice contracts"
---

Created:  2026-10-08 by Virtuozzo International GmbH
Updated:  2026-10-08 by Virtuozzo International GmbH

# ADR-0001: Money Representation, Precision, and Rounding

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Scope](#decision-scope)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Rules](#rules)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
  - [Review and Supersession](#review-and-supersession)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Major-Unit Decimals with Exact Fractions](#major-unit-decimals-with-exact-fractions)
  - [Minor Units Everywhere](#minor-units-everywhere)
  - [One Fixed-Precision Decimal Type Everywhere](#one-fixed-precision-decimal-type-everywhere)
  - [Binary Floating Point](#binary-floating-point)
- [Adoption](#adoption)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-bss-adr-exact-decimal-boundaries`

## Context and Problem Statement

Metering, pricing, charge calculation and accounting pass numbers between them.
Each may use a different numeric type.
Their contracts must preserve the value at every boundary.

A **major unit** is one euro or one dollar.
A **minor unit** defines the currency's posting increment.
For EUR, one minor unit is one cent.
A posted decimal amount of `12.34 EUR` is a valid multiple of that increment.
The amount stays in euros at every stage.

Usage has no currency, but its precision matters for money:

```text
amount = rate × quantity
```

An individual quantity may fit `Decimal` while a sum or product does not.
Also, fractions such as `1 / 3` have no exact finite decimal form.
Using the same decimal type everywhere cannot guarantee exact results.

We need shared rules for numeric types, limits, storage and rounding.
Stored inputs must reproduce the same invoice amount years later.

## Decision Scope

This ADR covers:

- SDK calls, REST fields and events that carry money or quantities.
- Calculations over those values.
- Their storage and text formats.
- Rounding and validation of a decimal invoice-line amount before posting.

Unbounded query totals have an explicit exception in rule 2.
They remain exact decimals but do not enter money calculations.

These numeric rules also apply to tax, FX and revenue recognition.
Their contract owners define the calculation and rounding policies.
Those policies must be explicit and preserve exact values until a declared rounding step.

## Decision Drivers

- **Replay:** stored inputs must produce the same result every time.
- **Precision:** rates can be smaller than a minor unit, such as `0.047 EUR`.
- **Clear limits:** each producer must publish the values it can send.
- **Currency:** EUR, JPY and KWD use 2, 0 and 3 minor digits respectively.
- **Exact storage:** writing and reading a value must not change it.
- **One posting step:** each invoice line is rounded once by its contract owner.
- **Consistent units:** prices, charges and postings all use major currency units.

## Considered Options

- Major-unit decimals, with exact fractions for intermediate calculations.
- Minor units everywhere, with a separate smaller unit for rates.
- One fixed-precision `rust_decimal::Decimal` type everywhere.
- Binary floating point.

## Decision Outcome

Express money in major currency units throughout the system, including posting.
Use Decimal for prices, finite charges and posted amounts.
Use an exact fraction for an intermediate result that has no finite decimal form.

The owner of the invoice-line contract rounds the exact total to the currency scale.
The result is a validated decimal amount in the same major units.
For example, `0.047 EUR` rounds to a posted amount of `0.05 EUR`.

Choose a decimal type wide enough for the published limits.
Declared business rounding remains part of the calculation, as rule 5 explains.

### Rules

#### 1. Send numbers as exact text

Money and quantities must not use `f32` or `f64`.
Send decimal values and integers as JSON strings, never JSON numbers.
An exact fraction uses two integer strings.

Reject a JSON number in these fields with `InvalidArgument`.
The error message must explain that the field requires a string.

#### 2. Publish the numeric form of each field

Contracts use two exact numeric forms:

| Form | Use |
|---|---|
| Finite decimal | Prices, rates, fees, thresholds, quantities and posted money |
| Exact fraction | Intermediate amounts that require non-terminating division |

**Finite decimals** use canonical decimal text under rule 6.
A Rust SDK uses `rust_decimal::Decimal` when the published limits fit it.
Consumers in other languages follow the text format and limits.

**Unbounded query totals are an explicit exception.**
An aggregate query may publish a wider decimal type, such as `BigDecimal`.
These values may exceed `Decimal` and still cross SDK and REST boundaries.
They are for queries and operator views, not money calculations.

**Exact fractions** use `{numerator, denominator}` as integer strings.
Reduce the fraction and keep the denominator positive.
The producer must publish limits for both integers.

A field declared as a fraction keeps that form even when its value is an integer.
The producer and consumer must agree on the form before implementation.

**Money** carries a decimal amount, currency code and `currency_scale`.
The amount is always in **major currency units**, including after posting:

```json
{
  "amount": "12.34",
  "currency": "EUR",
  "currency_scale": 2
}
```

**PostedMoney** uses the same fields and decimal representation.
Its constructor validates the currency, scale, amount limits and posting increment.

It accepts `0.05 EUR` at scale 2 and rejects `0.047 EUR`.
It must reject an invalid value rather than round it silently.
The same checks apply when reading a posting request from the wire.

An intermediate fraction also carries currency and scale, and is in major units.
A posting request must carry a validated decimal, never a fraction.
Quantities carry their metering unit; they have no currency.

Do not expose integer minor units, nano-minor or micro-minor amounts in these contracts.
A field's numeric form is part of its versioned contract.

#### 3. Publish limits before choosing a type

Each producer publishes:

- For decimals: maximum significant digits, fractional digits and magnitude.
- For fractions: maximum digits in the numerator and denominator.
- For computed quantities: output limits and any declared rounding rules.

Consumers must allow for the operations they perform:

- Sums can grow with the number of terms.
- Multiplication can need more digits and decimal places than either input.
- Fraction numerators and denominators can grow during calculation.

The consumer's design must state its limits and explain why they are enough.
Reject an out-of-range result with a named error before storing or sending it.

A charge calculator reads bounded usage entries and aggregates them itself.
It does not use unbounded query totals.

#### 4. Keep calculations exact

Addition, multiplication, aggregation and proration must preserve every digit.
Rule 5 lists the allowed business rounding operations.

Use `rust_decimal::Decimal` only when the exact result is proven to fit.
That type holds 28 significant digits in total, with a scale of at most 28.
Use checked operations and return a named error on overflow.

Checked operations alone are not proof of exactness.
The design must also show that the operation will not reduce precision.

Use an exact fraction for a non-terminating result such as `1 / 3`.
A wider decimal is suitable only when the exact result is finite and fits.
Never replace an exact result with a truncated quotient.

A wide internal type must convert exactly to the published form in rule 2.
Unbounded query decimals may keep their wide type across the boundary.

The charge calculator must handle wide quantity products and exact proration.
Its design must choose types and prove the limits before implementation.
Keep all money calculations in major units, regardless of the internal numeric type.

#### 5. Preserve declared business rounding

Business rounding defines what is billed.
It is allowed when a published rule requires it.
Rounding caused only by a numeric type's limits is not allowed.

A derived-quantity contract may declare:

- Ceiling, floor or rounding operations inside a formula.
- An output scale and rounding mode for each calculation interval.

The contract defines the order of these operations and how interval outputs combine.
The evaluator must follow that order exactly.

A tariff may also define a minimum billable unit.
When it requires rounding up an aggregated measure:

1. Merge or aggregate the measure over the declared scope.
2. Round it up once to the configured billing unit.
3. Then apply the rate and tier rules.

For example, five minutes at `per_hour` bills one hour.
Twelve five-minute samples merged into one hour still bill one hour.
Package block counting also keeps its declared ceiling rule.

These rules change the billable quantity by design.
They do not authorize early rounding of money to cents.

#### 6. Store exact values in a canonical form

Use an exact database type sized for the published limits.
For PostgreSQL this can be `NUMERIC(p, s)`.
Store posted amounts as exact decimals in major units too.
Validate before writing; database scale coercion must not round an invalid posting.
Use decimal text where the ORM cannot preserve the backend's decimal value.
For example, a SQLite/SeaORM path through `f64` requires an exact text alternative.

Use the same canonical decimal text on the wire, in text storage and in hashes:

- Use `-` only for a negative value; never use `+`.
- Omit leading zeros, except the `0` before a decimal point.
- Include a decimal point only when a fractional part remains.
- Remove trailing zeros from the fractional part.
- Do not use exponents or thousands separators.
- Write every zero as `0`.

For example, `10.000`, `10.0` and `10` normalize to `10`.
They must produce the same digest.
Bound the text length by the producer's published limits.
Unbounded query totals use their separate query contract.

A canonical fraction is reduced, with a positive denominator.
Put the sign on the numerator; write zero as `0 / 1`.

#### 7. Round each invoice line once

The owner of the invoice-line contract defines and publishes the **invoice line key**.
That owner also publishes the rounding and correction rules defined below.
These responsibilities stay together when components are renamed or split.

The key contains at least:

- Billing group and subscription line.
- Item and dimension value.
- Charge kind, currency, stored `currency_scale` and unit.
- Accounting and tax classes that cannot share a line.

The component forming invoice lines follows this order:

1. Collect the exact contributions with the same key.
2. Include discounts, proration and any minimum-fee top-up.
3. Sum the contributions without rounding.
4. Round the sum with `HALF_EVEN` at the stored currency scale.
5. Validate the decimal result as `PostedMoney`, then post it in major units.

`HALF_EVEN` selects the even neighbour when the value is exactly halfway.
At scale 2, `0.005` rounds to `0.00`, and `0.015` rounds to `0.02`.
The rule applies to positive and negative totals.
The posting receiver validates the amount and never rounds it again.

| Contributions | Grouping | Posted amount |
|---|---|---|
| `0.005 + 0.005 EUR` | One line | `0.01 EUR` |
| `0.005 EUR` each | Two lines | `0.00 EUR` each |

The line key therefore affects the amount and must be part of the contract.
Tax and FX owners decide whether their rules use the exact or rounded line.

An order preview may show a rounded total before posting.
It must be marked as non-authoritative.
An SDK returning that display total also carries its exact value.
A preview consumer may persist the rounded decimal total in major units.

#### 8. Round a corrected total before subtracting postings

Recalculate the original line from its corrected exact contributions.
Keep its original grouping and stored currency scale.
Then calculate the adjustment:

```text
corrected_amount = round_currency(corrected_line_sum, currency_scale, HALF_EVEN)
adjustment_amount = corrected_amount - cumulative_posted_amount
```

All amounts in this formula are in major currency units.
`round_currency` returns a decimal rounded to the requested currency scale.
`cumulative_posted_amount` includes the original posting and all prior notes.

Subtract exactly and validate the adjustment as `PostedMoney`.
Post it as a credit or debit note, with no further rounding.
Reject a result outside the posting contract's published limits.
Do not round the exact difference from the old posted amount.

Example at EUR scale 2:

| Step | Amount |
|---|---|
| Original exact total | `0.006 EUR` |
| Original posting | `0.01 EUR` |
| Corrected exact total | `0.005 EUR` |
| Corrected rounded total | `0.00 EUR` |
| Credit adjustment | `-0.01 EUR` |

Rounding `0.005 - 0.01` would give zero and leave the wrong balance.
Posted amounts are used only to find the adjustment, never to rate usage again.

#### 9. Preserve currency and scale

Validate new money values against the producer's currency table.
For example, an EUR value with `currency_scale = 3` is invalid.
Reject a mismatch with `InvalidArgument`.

`currency_scale` defines the posting increment, not the precision of a rate.
An EUR rate of `0.047` has three decimal places and `currency_scale = 2`.
It is valid as a rate; a posting at that scale must be a multiple of `0.01`.
Trailing fractional zeros do not change validity: `12.340` equals `12.34`.

Store the currency scale with each money value, including decimals and fractions.
Replay uses that stored scale, even if the currency table later changes.

Arithmetic requires matching currency and scale.
Reject mismatches with a named error; never convert implicitly.
An SDK adapter may change the encoding but not the currency, scale or numeric form.

### Consequences

- Producers publish value limits and reject inputs outside them.
- Consumers choose numeric types from those limits and their own operations.
- The invoice-line contract owns grouping, rounding and cumulative corrections.
- The posting receiver accepts validated major-unit decimals without rounding again.
- Query APIs may expose wide decimals under their separate query contracts.
- Preview consumers keep rounded display totals clearly marked as estimates.
- SDK owners should keep value, currency and scale together in money types.
  Constructors should reject invalid combinations.

Money and quantity contracts must follow these rules.
Each contract publishes its numeric form, units and limits.
Concrete digit limits and storage sizes belong in those contracts and designs.

### Confirmation

**Encoding and storage**

- JSON numbers in money or quantity fields are rejected with `InvalidArgument`.
- `10.000` and `10` normalize to the same text and digest.
- Canonical wire parsers reject exponents, leading `+`, leading zeros and `-0`.
- Storage preserves all accepted digits on every supported backend.
- Unbounded query values above `Decimal`'s limit still round-trip exactly.

**Limits and arithmetic**

- Test each producer's limit and a value just beyond it.
- A product needing scale 31 stays exact in a wider type or returns a named error.
- `1 / 3` remains an exact fraction.
- Large sums remain exact or return the documented range error.
- A posting outside the declared decimal limits fails without rounding or truncating.

**Declared quantity rules**

- Preserve declared ceiling, floor and rounding operations inside formulas.
- Test output rounding and aggregation in the order the quantity contract defines.
- Five minutes at `per_hour` bills one hour.
- Twelve merged five-minute samples at `per_hour` also bill one hour.

**Posting and corrections**

- A scale-2 EUR posting accepts `0.05` and rejects `0.047` without rounding it.
- Apply the same posting checks to SDK constructors, wire requests and storage writes.
- Test both invoice groupings from rule 7.
- Keep contributions with different stored scales in separate lines
  during replay and corrections.
- Test positive and negative `HALF_EVEN` ties.
- Correcting `0.006 EUR` to `0.005 EUR` posts a `-0.01 EUR` adjustment.
- A second correction includes prior notes in the posted total.
- Replaying stored inputs reproduces the posted amount exactly.

**Currency and contracts**

- Reject EUR with scale 3 and arithmetic between EUR and USD.
- Replay with the stored scale after a currency-table change.
- Prices, charges, previews and postings all express money in major units.
- Each field declares its form, unit, limits and allowed rounding.
- Money fields also declare their currency and scale.

### Review and Supersession

Review this ADR when a producer changes its limits or a field changes form.
A smaller quantity scale may reduce the required arithmetic width.
It does not make fractions such as `1 / 3` finite decimals.

A future move to decimal-only arithmetic must prove that all supported results fit exactly.
Record that change in a superseding ADR.

## Pros and Cons of the Options

### Major-Unit Decimals with Exact Fractions

- Uses the same money units and decimal representation through posting.
- Preserves authored prices and accepted usage values.
- Keeps currency and scale explicit.
- Gives the invoice-line contract one owner for rounding and corrections.
- Supports exact replay and cumulative corrections.
- Requires wider types, published limits and range checks in some components.
- Requires a validated posting type to prevent fractional minor-unit amounts.
- Exact decimal storage can cost more than integer storage.

### Minor Units Everywhere

- Simple integer storage and direct compatibility with accounting entries.
- Rates smaller than one minor unit need another scale.
- Consumers must convert that scale using the currency.
- A fixed smaller unit still cannot represent every proration fraction.

### One Fixed-Precision Decimal Type Everywhere

- Simple SDK types and fewer conversions.
- Individual prices and quantities often fit `rust_decimal::Decimal`.
- Large sums and products may exceed its precision or range.
- Non-terminating fractions cannot be represented exactly.
- Narrowing input limits alone does not solve exact division.

### Binary Floating Point

- Widely available in programming languages.
- Cannot represent many ordinary decimal values exactly.
- Can lose digits during parsing, arithmetic or storage.
- Does not meet the exact-value requirement.

## Adoption

This section maps the roles to the current gears.
It records implementation work; it does not add platform-wide numeric limits.
Renaming a gear or moving a role does not change the rules above.

| Role | Current or planned owner |
|---|---|
| Raw usage producer and aggregate query API | usage-collector |
| Derived-quantity contract | products |
| Price and rate producer | pricing |
| Charge calculator | rating |
| Invoice-line contract and rounding | Billing (planned) |
| Posting receiver | ledger |
| Order preview consumer | orders-lifecycle |

**Usage-collector** provides `Decimal` entries and `BigDecimal` query totals.
Its [quantity schema][usage-api] owns the entry limits:
28 significant digits, 28 fractional digits and magnitude below `10^28`.

**Products** owns derived formulas and their declared rounding.
Its [quantity contract][products-prd] and [SDK][products-sdk] own these rules.
Its output scale limit is 12.
The contract must also state the full output magnitude limit.

**Pricing** publishes major-unit decimals and uses exact storage.
It must define rate and amount limits in its own contract.
A 12-place fractional limit is a proposal for that owner to assess.
The `minimum_fee` limit follows the currency scale.

**Rating and Billing** must define the exact charge-delivery contract together.
Fractions in major units are the proposed form for that boundary.
Their [design set][rating-design] must justify the numeric limits.
Define bounds for quantities, numerators and denominators there.
Compare exact price formulas with the producer's golden test cases.

**Billing** must publish the line key and implement rules 7 and 8.

**Ledger** accepts `PostedMoney` as a decimal amount in major currency units.
It validates the posting increment, currency, scale and published amount limits.
It stores the decimal amount exactly and does not round incoming postings.

**Orders-lifecycle** represents display totals as major-unit decimals marked as estimates.

These owners must verify the full path with shared golden test cases:
usage, price, proration, line grouping, posting and successive corrections.

## More Information

Stripe also exposes integer amounts and decimal rate strings.
Its `unit_amount_decimal` is in cents or the local equivalent,
with up to 12 fractional places.
This ADR instead expresses decimal money in major units.
See the [Stripe price contract][stripe-prices].

## Traceability

- **Usage-collector:** [quantity limits][usage-api],
  [quantity requirements][usage-prd] and [SDK numeric types][usage-sdk].
- **Products:** [derived formulas][products-sdk] and [product rules][products-prd].
- **Pricing:** [read models][pricing-read], [invoice terms][pricing-terms],
  [digests][pricing-digest] and [price arithmetic][pricing-money].
- **Rating:** [design set][rating-design] (evaluation core, billing granularity, Billing delivery).
- **Ledger:** [posting types][ledger-sdk] and [ledger design][ledger-design].
- **Orders-lifecycle:** [resolved display totals][orders-design].
- **Platform:** [portable storage][database-adr] and [error categories][errors-adr].

[stripe-prices]: https://github.com/stripe/stripe-go/blob/master/price.go
[usage-api]: ../../../system/usage-collector/docs/usage-collector-v1.yaml
[usage-prd]: ../../../system/usage-collector/docs/PRD.md
[usage-sdk]: ../../../system/usage-collector/usage-collector-sdk/src/models.rs
[products-sdk]: ../../products/products-sdk/src/derived.rs
[products-prd]: ../../products/docs/PRD.md
[pricing-read]: ../../pricing/pricing-sdk/src/read.rs
[pricing-terms]: ../../pricing/pricing-sdk/src/terms.rs
[pricing-digest]: ../../pricing/pricing-sdk/src/digest.rs
[pricing-money]: ../../pricing/pricing/src/domain/money.rs
[rating-design]: ../../rating/docs/DESIGN.md
[ledger-sdk]: ../../ledger/ledger-sdk/src/posting.rs
[ledger-design]: ../../ledger/docs/DESIGN.md
[orders-design]: ../../orders-lifecycle/docs/DESIGN.md
[database-adr]: ../../../../docs/arch/database/ADR/0001-cpt-cf-database-adr-object-namespacing.md
[errors-adr]: ../../../../docs/arch/errors/ADR/0001-cpt-cf-adr-canonical-error-categories.md
