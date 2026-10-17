---
status: proposed
date: 2026-10-11
---

# ADR-0003: Evaluator Linked at Build Time Behind a Trait

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [(a) Evaluation logic in the gear](#a-evaluation-logic-in-the-gear)
  - [(b) Evaluator selected at runtime](#b-evaluator-selected-at-runtime)
  - [(c) Evaluator linked at build time](#c-evaluator-linked-at-build-time)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-license-enforcement-adr-compile-time-evaluator`

## Context and Problem Statement

The evaluator decides which grants of the tenant satisfy a check. It selects the grants whose binding matches the
request, orders them, and decides for each one whether it passes on its fields at the evaluation instant. This logic
can change apart from the domain model, lifecycle, and store of the gear. It can be Rust code or a policy in a policy
language.

In an untrusted build, the operator controls the configuration, the registry, and the database. Subject types are 
registered in GTS at runtime. A tenant can hold tens of thousands of grants on one resource.

How do we bind the evaluator to the gear, so that its logic can change without a change of the gear, the operator
cannot replace it, and a check reads only the grants that can match?

## Decision Drivers

* In an untrusted build, an evaluator that the operator can replace is a grant that the operator can widen (PRD §3).
* The evaluation logic changes without a change of the gear.
* Subject types come from GTS at runtime. A new type needs no new build
  (`cpt-cf-license-enforcement-fr-subject-scope`, `cpt-cf-license-enforcement-fr-type-references`).
* The check reads only the candidates that can match, through indexes (`cpt-cf-license-enforcement-nfr-check-latency`).
* The check path is local and changes no grant, and the same inputs give the same decision
  (`cpt-cf-license-enforcement-constraint-local-check-path`, `cpt-cf-license-enforcement-nfr-determinism`).

## Considered Options

* (a) The evaluation logic is code of the gear.
* (b) The evaluator is selected at runtime: from configuration or from a GTS plugin instance in the registry.
* (c) The evaluator is a crate that the binary links at build time, behind a trait, as the admission plugin
  (`cpt-cf-license-enforcement-adr-compile-time-admission`).

## Decision Outcome

Chosen option: **(c)**.

The binary links exactly one evaluator crate. The crate registers itself at link time. At init, the gear does not
start if it finds zero evaluators or more than one. No configuration key, environment variable, request, artifact
field, or registry entry changes the evaluator.

The trait has these operations:

- The candidate filter takes the request and returns an ordered list of binding filters. Each filter is a GTS type
  pattern and a binding id. The store reads the grants that match any filter. The list order sets the candidate
  order.
- The evaluation takes the request, a candidate, its root, and the evaluation instant. It returns pass, pass in
  grace, or a denial reason. It evaluates the fields that it gets: the stored fields, or the root fields from `verify`.
- The validation checks conditions or recipient rules against a contract or projection schema, at write time.
- The match evaluates conditions or recipient rules against an object, for example the recipient projection at
  delegation.

The filter is the boundary between the evaluator and the store. The evaluator does not see the store, and the store
does not know the logic of the evaluator. A GTS type pattern covers the types that derive from it, so a new Subject
type falls under an existing filter.

The crate decides how it expresses its logic: Rust code, or a policy that the crate embeds. The evaluator is a pure
function. It reads nothing, calls nothing, and writes nothing.

Option (a) makes each change of the logic a change of the gear. Option (b) lets the operator replace the evaluator,
and it adds a registry call to the check path.

### Consequences

* The gear ships a default evaluator crate. DESIGN §3.1 describes what it matches.
* A new evaluation logic is a new evaluator crate and a new build. The gear does not change.
* The denial reasons stay a closed enum in the SDK. An evaluator returns only these reasons.
* The evaluator defines the form of conditions and recipient rules. It validates them at write time and evaluates
  them at check time and at delegation. Thus one implementation does both.
* The store indexes the grants by tenant, resource type, binding type, and binding id.
* The conformance suite of the gear runs with each shipped evaluator crate.

### Confirmation

* A binary with zero evaluator crates, or with two, does not start.
* No configuration key, environment variable, or registry entry changes the evaluator.
* With the default evaluator, a Subject type that derives from a matched type, and that is registered after the gear
  starts, matches without a new build.
* The query plan of the candidate read uses the index for a tenant with 50 000 grants on one resource.

## Pros and Cons of the Options

### (a) Evaluation logic in the gear

* Good, because there is no trait to maintain.
* Bad, because each change of the logic is a change of the gear.

### (b) Evaluator selected at runtime

* Good, because a change of the logic needs no new build.
* Bad, because in an untrusted build the operator controls configuration and registry, and can widen every grant.
* Bad, because selection from the registry puts a registry call on the check path.

### (c) Evaluator linked at build time

* Good, because the logic changes without a change of the gear, and the operator cannot replace it.
* Good, because the filter keeps the candidate read on indexes, and a new Subject type needs no new build.
* Neutral, because a change of the logic is a new build.
* Bad, because the gear maintains a second plugin trait.

## More Information

DESIGN §3.1 contains the evaluation order. DESIGN §3.3 contains the trait.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses these requirements and design elements:

* `cpt-cf-license-enforcement-fr-subject-scope` — the filter matches a subject grant and a grant on the tenant Subject
  type, for any Subject type in GTS.
* `cpt-cf-license-enforcement-nfr-check-latency` — the check reads only the candidates that match a filter.
* `cpt-cf-license-enforcement-component-evaluator` — the component that this decision makes a linked crate.
