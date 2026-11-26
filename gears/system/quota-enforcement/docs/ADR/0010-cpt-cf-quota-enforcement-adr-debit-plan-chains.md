---
status: accepted
date: 2026-11-18
---

# Debit Plan of Chains

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Chains](#a-chains)
  - [(b) Flat plan, proportional unwinding](#b-flat-plan-proportional-unwinding)
  - [(c) Flat plan, partial unwinding only for one entry](#c-flat-plan-partial-unwinding-only-for-one-entry)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-quota-enforcement-adr-debit-plan-chains`

## Context and Problem Statement

An engine returns a debit plan: the amount each quota is charged. Today the plan is a map from quota to amount, each
amount at most the requested amount, with no rule for their sum. One plan can therefore mean a split (a cascade from
a primary pool to a fallback, or a weighted split: the amounts add up to the requested amount), an AND (each quota
counts the whole operation), or a mix. The plan does not say which, and a map loses the order of a cascade.

Two operations must take back part of a charge: a lease commit below the held amount, and a return of part of a charge
(ADR-0011). Today a commit splits the unused part proportionally. How does QE take back part of a multi-quota charge?

## Decision Drivers

* Taking back part of a charge must follow how the charge was made, without rounding.
* Commit and return take back amounts the same way.
* A `cel` Policy can express a cascade, an AND, and their mix.

## Considered Options

* (a) A plan is a list of chains; each chain is unwound in reverse order.
* (b) Keep the flat plan; take back proportionally to each quota's share.
* (c) Keep the flat plan; allow a partial take-back only for a single-entry plan.

## Decision Outcome

Chosen option: (a).

* A debit plan is a list of **chains**. A chain is an ordered list of entries `{quota, amount}`, primary pool first;
  its amounts add up to exactly the requested amount. Several chains are an AND: each counts the whole operation. A
  quota appears at most once in a plan. QE rejects a plan that breaks these rules as an engine failure.
  * The default engine returns one chain with one entry.
  * A cascade or a split is one chain with several entries; an AND is one chain per quota.
* **Unwinding** takes back an amount X from a charge: in every chain, X is taken from the entries in reverse order,
  last entry first, each entry at most what it still holds. Each chain gives back exactly X.
* A lease commit below the held amount unwinds the unused part from the lease's holds. A return unwinds the returned
  amount from the charge (ADR-0011).
* The `cel` engine returns `{"debit_plan": [[{id, amount}, ...], ...]}`.
* Charge records and lease holds keep each entry's chain and position.

### Consequences

* The engine contract and the `cel` output format change; a stored `cel` Policy must return chains.
* A weighted split gives back from its last entry first, not in proportion. A chain kind that unwinds proportionally
  can be added when a Policy needs it.

### Confirmation

Tests: plan validation (chain sum, a quota twice), unwinding of a cascade, an AND, and a mix, a commit below the held
amount, a full and a partial return.

## Pros and Cons of the Options

### (a) Chains

* Good, because the plan states its meaning and unwinding is exact.
* Bad, because the engine contract and every `cel` Policy change.

### (b) Flat plan, proportional unwinding

* Good, because no contract changes.
* Bad, because a partial return of a split gives back up to one unit less per entry, and a cascade gives back from
  every pool at once.

### (c) Flat plan, partial unwinding only for one entry

* Good, because it is the least code.
* Bad, because one `cel` Policy turns a valid partial return into an error.

## More Information

Debit Plan and Quota Cascade: PRD §1.4 Glossary.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses:

* `cpt-cf-quota-enforcement-fr-quota-resolution-engine`
* `cpt-cf-quota-enforcement-fr-lease-commit`
* `cpt-cf-quota-enforcement-fr-return`
* `cpt-cf-quota-enforcement-adr-evaluation-engine`
