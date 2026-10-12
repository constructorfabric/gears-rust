# ADR-0002: Price Chains per Dimension Value with Default Fallback

**ID**: `cpt-cf-bss-pricing-adr-price-chains-per-dimension-value`

- [ ] `p1` - **ADR implementation status**

**Status:** accepted 2026-09-25 · **Deciders:** product owner · **Source:** spec §2 decisions 4 and 16; §5; §7.1.

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

Region market prices cannot express one optional extensible dimension with independent windows and safe default fallback. Closing one region must not close another, and a temporary override must not permanently fork the default.

## Decision Drivers

`cpt-cf-bss-pricing-fr-dimension-registry`, `cpt-cf-bss-pricing-fr-chain-windows`, `cpt-cf-bss-pricing-fr-temporary-pair`, `cpt-cf-bss-pricing-fr-pair-guard`. The approved PriceBook model is the content authority. The decision must hold under tenant isolation,
concurrent writes and either supported database, and must remain explainable to operators and reviewers.

## Considered Options

1. Keep a region market axis.
2. Use value chains with a default.
3. Copy the default into every dimension value.

## Decision Outcome

Chosen: **Use value chains with a default**. An entry optionally selects one tenant dimension key. Prices form independent chains by nullable dim_value. Resolve prefers a value chain at the date, then default. Approval normalizes windows per chain; default tail stays open, while a value tail may end explicitly. A temporary override on a value without its own chain is one closed price; an existing chain gets a pair whose return copies versionAt at the end date.

### Consequences

A default chain is optional and plan coverage checks every value. Registry values with prices cannot be removed. Usage successor model/package/unit guards apply per chain. Copying a default return onto a formerly unowned value would destroy future fallback and is forbidden.

### Confirmation

Approve prices for two values and prove only the selected predecessor closes. End a value tail and observe default fallback. A temporary price for a previously unowned value creates no return price. Changed usage model/package/unit is CHAIN_MODEL_CHANGED. These are implementation acceptance conditions; Part 2a replaces documents only.

## Pros and Cons of the Options

A region-only axis cannot accommodate another registered key. Value chains preserve independent windows and fallback but require per-value coverage checks. Eager default copies avoid fallback reads while freezing money that should continue following the default.

## More Information

[PRD](../PRD.md), [DESIGN](../DESIGN.md), [DECISIONS](../DECISIONS.md).
Source: `docs/superpowers/specs/2026-09-24-pricebook-model-design.md` in the main checkout, §2 decisions 4 and 16; §5; §7.1.
The superseded set remains on `bss/products-backup` and in history.
