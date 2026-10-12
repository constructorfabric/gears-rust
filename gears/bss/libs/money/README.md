# `money`

> One money representation for the BSS gears: a validated decimal in major units with
> its currency scale, exact fraction arithmetic, HALF_EVEN rounding at the declared
> points, proportional allocation, and the canonical decimal text used on the wire, in
> storage and in hashes.

## What it gives you

- `PostedMoney { amount: Decimal, currency: CurrencySpec { code, scale } }` — an amount
  that is already a multiple of the currency's posting increment. `12.34 EUR` at scale 2
  is accepted; `0.047` is refused with `InvalidPostingIncrement`. Limits: 28 significant
  digits, scale 0–28. Nothing in this crate rounds an input.
- `parse_decimal` / `canonical_decimal` — the one text form. Input accepts fractional
  trailing zeros; output never has them. Exponents, `+`, leading zeros and `-0` are
  refused.
- `exact::ExactAmount` (feature `exact`) — reduced fractions over `BigInt` for
  intermediate arithmetic (thirds stay thirds). Narrow to money with
  `into_posted_exact` (must land on the grid) or `round_half_even` (one rounding, ties
  to even). `exact::sum_posted` adds postings of one currency and scale exactly.
- `allocate::allocate(total, weights, Residual)` (feature `exact`) — proportional
  shares rounded once each, with the exact residual assigned to the last or the largest
  slot.
- `serde_text` (feature `serde`) — `#[serde(with = "bss_money::serde_text")]` for a
  `Decimal` field stored or sent as canonical text; `serde_text::option` for
  `Option<Decimal>`.

## What it does not do

- It holds no currency table. A gear takes scales from its registry or provisioning and
  builds `CurrencySpec` from them.
- It does not choose when to round. Each gear names its rounding points and calls
  `round_half_even` there; everything else stays exact.
- It carries no REST or storage types. Gears wrap `PostedMoney` in their own DTOs and
  entities and keep the text canonical with `canonical_decimal`.

## Who uses it

- `ledger` and `ledger-sdk`: postings, balances, allocation, FX, recognition (the SDK
  re-exports the money types as part of its contract).
- `rate-provider-sdk`: exact parsing of provider quotes.
- `pricing`: the canonical decimal text of stored commercial terms and prices.
