---
status: proposed
date: 2026-03-26
owner: SDK architecture team
scope: modules/serverless-sdk
priority: p0
---
<!--
=============================================================================
ARCHITECTURE DECISION RECORD (ADR) — based on MADR format
=============================================================================
RULES:
 - ADRs represent actual decision dilemma and decision state
 - DESIGN is the primary artifact ("what"); ADRs annotate DESIGN with rationale ("why")
 - Use single ADR per decision

STANDARDS ALIGNMENT:
 - MADR (Markdown Any Decision Records)
 - IEEE 42010 (architecture decisions as first-class elements)
 - ISO/IEC 15288 / 12207 (decision analysis process)
==============================================================================
-->

# ADR — `WorkflowHandler` as a Supertrait of `Handler` (Not an Independent Trait)


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: `WorkflowHandler<I, O>: Handler<I, O>` supertrait](#option-a-workflowhandleri-o-handleri-o-supertrait)
  - [Option B: Independent `WorkflowHandler<I, O>` trait](#option-b-independent-workflowhandleri-o-trait)
  - [Option C: `WorkflowHandler<I, O>` extends `Handler<I, O>` with default `compensate`](#option-c-workflowhandleri-o-extends-handleri-o-with-default-compensate)
- [More Information](#more-information)
- [Non-Applicable Domains](#non-applicable-domains)
- [Review Conditions](#review-conditions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-serverless-sdk-core-adr-workflow-supertrait`

## Context and Problem Statement

The Serverless Runtime's unified function model (`cpt-cf-serverless-runtime-principle-unified-function`)
treats workflows as a specialisation of functions: every workflow is also a callable
function. The SDK must express this in Rust's type system. Two primary structural
options exist:

1. **Supertrait**: `WorkflowHandler<I, O>: Handler<I, O>` — implementing
   `WorkflowHandler` requires implementing `Handler` first; the workflow exposes
   both a `call` method (the forward execution path) and a `compensate` method
   (the saga rollback path) through a single type.
2. **Independent trait**: `WorkflowHandler<I, O>` is a standalone trait with both
   `call` and `compensate` methods declared directly, without inheriting from `Handler`.

A third option extends this: providing a default `compensate` implementation on
`WorkflowHandler` so that `Handler` implementors opt into compensation incrementally.

Which structural relationship should the SDK use between `Handler` and `WorkflowHandler`?

## Decision Drivers

* **[P1]** The Serverless Runtime's domain model declares that every workflow is a
  function (`cpt-cf-serverless-runtime-principle-unified-function`); the SDK type
  system must enforce this constraint, not merely document it — structural equivalence
  is a correctness requirement.
* **[P1]** Adapters that hold a `Box<dyn WorkflowHandler<I, O>>` must also be able to
  call `call` (the forward path) on the same object without a second trait object or
  cast — a single vtable for both operations simplifies adapter dispatch.
* **[P2]** Compensation is a mandatory contract for durable workflows, not an optional
  extension (`cpt-cf-serverless-sdk-core-fr-workflow-handler-trait`); providing a
  default no-op `compensate` would allow workflows to silently skip rollback on failure,
  violating the saga pattern.
* **[P2]** Handler authors must be able to unit-test both `call` and `compensate` in
  isolation on the same struct — a single implementing type satisfies both test paths
  without additional adapter infrastructure.
* **[P3]** The supertrait relationship must not prevent adapters from registering a
  `Box<dyn Handler<I, O>>` (plain function) separately from a `Box<dyn WorkflowHandler<I, O>>`
  (workflow) — the two categories must be distinguishable at the adapter registration layer.

## Considered Options

* **Option A**: `WorkflowHandler<I, O>: Handler<I, O>` supertrait — `WorkflowHandler`
  extends `Handler`; implementing `WorkflowHandler` requires a prior `Handler` impl;
  `compensate` is an additional required method with no default.
* **Option B**: Independent `WorkflowHandler<I, O>` — a standalone trait that redeclares
  both `call` (identical signature to `Handler::call`) and `compensate`; no supertrait
  relationship; `Handler` and `WorkflowHandler` are fully separate.
* **Option C**: `WorkflowHandler<I, O>: Handler<I, O>` with a default `compensate` —
  same supertrait relationship as Option A, but `compensate` has a default implementation
  (e.g., returning `Ok(())` unconditionally) so any `Handler` implementor can
  trivially implement `WorkflowHandler`.

## Decision Outcome

Chosen option: **Option A: `WorkflowHandler<I, O>: Handler<I, O>` supertrait**,
because it makes the `workflow ⊂ function` relationship a compile-time guarantee rather
than a documentation convention. A single implementing type satisfies both the
forward-call and compensation obligations, which aligns with the Serverless Runtime's
invocation model (both paths are standard invocations with different input shapes).
The required — non-defaulted — `compensate` method enforces that compensation is never
silently absent from a durable workflow; function authors must explicitly implement it.

### Consequences

* Any type implementing `WorkflowHandler<I, O>` must also implement `Handler<I, O>`;
  the compiler enforces this. This is intentional: a durable workflow that lacks a
  forward execution path is not a valid workflow.
* A type implementing `WorkflowHandler<I, O>` automatically satisfies any bound
  requiring `Handler<I, O>`. Adapters can store a `Box<dyn WorkflowHandler<I, O>>`
  and call `call` on it via the supertrait without a separate vtable or cast.
* Function authors cannot implement `WorkflowHandler` without implementing `Handler`.
  This means unit-testing compensation requires constructing a struct that also
  satisfies `Handler`, even if the test only exercises `compensate`. The test overhead
  is minimal: `call` can be implemented as `unimplemented!()` in compensation-only
  test scenarios.
* A plain `Handler<I, O>` implementation (stateless function) can never be passed
  where a `WorkflowHandler<I, O>` is required. This is correct: adapters that dispatch
  compensation invocations must only do so for registered workflow types.
* There is no way to add compensation to an existing `Handler` without refactoring it
  to implement `WorkflowHandler`. This is intentional: mixing forward and compensation
  dispatch through the same trait without the supertrait relationship would create
  ambiguity about whether a given `Handler` supports compensation.

### Confirmation

* `workflow.rs` declares `pub trait WorkflowHandler<I, O>: Handler<I, O>` — verified
  by inspection.
* `compensate` has no default implementation — verified by inspecting `workflow.rs`;
  any `impl WorkflowHandler` that omits `compensate` must fail to compile.
* Integration test: a struct implementing only `Handler<I, O>` (not `WorkflowHandler<I, O>`)
  must not satisfy a `WorkflowHandler` bound — confirmed by a negative compile test
  (`should_not_compile` or `trybuild` test).
* `Box<dyn WorkflowHandler<I, O>>` is usable as `Box<dyn Handler<I, O>>` via
  supertrait coercion — confirmed by an adapter dispatch test.

## Pros and Cons of the Options

### Option A: `WorkflowHandler<I, O>: Handler<I, O>` supertrait

`WorkflowHandler` extends `Handler`; `compensate` is required with no default.

* Good, because the `workflow ⊂ function` relationship is a compile-time guarantee —
  the type system enforces what the domain model requires.
* Good, because a `Box<dyn WorkflowHandler<I, O>>` satisfies `Box<dyn Handler<I, O>>`
  via supertrait coercion — adapters call `call` through a single vtable.
* Good, because compensation is mandatory — no workflow silently ships without rollback logic.
* Good, because both forward and compensation paths are on the same implementing struct —
  `call` and `compensate` share access to the same `self` fields (e.g., shared clients,
  configuration), which is the natural authoring model for saga rollback.
* Neutral, because unit-testing compensation in isolation requires a minimal `call`
  implementation on the test struct. Accepted: the overhead is one method stub.
* Bad, because a function author who wants to add compensation to an existing
  `Handler` must refactor to `WorkflowHandler`. Accepted: durable workflow authoring
  is a deliberate architectural choice, not a casual addition.

### Option B: Independent `WorkflowHandler<I, O>`

A standalone trait; `call` and `compensate` declared independently; no supertrait.

* Good, because `WorkflowHandler` can be implemented without a separate `Handler`
  impl block — the trait is self-contained.
* Good, because adapters can implement workflow dispatch without a supertrait
  dependency on `Handler`.
* Bad, because the `workflow ⊂ function` relationship from the runtime's domain model
  is expressed only in documentation, not in the type system — drift risk over time.
* Bad, because a `Box<dyn WorkflowHandler>` cannot be coerced to `Box<dyn Handler>`
  — code that accepts plain `Handler` objects cannot accept a workflow handler without
  a separate `Handler` registration or an explicit cast, breaking the unified
  dispatch model.
* Bad, because `call` is duplicated in two traits with identical signatures — a
  maintenance hazard; any signature change to `Handler::call` must be mirrored in
  `WorkflowHandler::call`.
* Bad, because it weakens the invariant that all registered workflows are also valid
  function invocations — the Serverless Runtime's unified invocation surface
  (`cpt-cf-serverless-runtime-principle-unified-function`) requires this.

### Option C: `WorkflowHandler<I, O>: Handler<I, O>` with default `compensate`

Same supertrait relationship as Option A, but `compensate` returns `Ok(())` by default.

* Good, because any `Handler` implementor can trivially opt into `WorkflowHandler`
  by adding a single `impl WorkflowHandler<I, O> for MyHandler {}` block.
* Good, because the supertrait relationship is preserved — no code duplication.
* Bad, because a workflow with the default `compensate` silently performs no rollback
  on failure. The platform will mark the invocation as `compensated` even though
  nothing was rolled back — a silent correctness failure with no compile-time warning.
* Bad, because the PRD requires that compensation is a mandatory contract for durable
  workflows (`cpt-cf-serverless-sdk-core-fr-workflow-handler-trait`); a default no-op
  contradicts this requirement.
* Bad, because it creates a false sense of safety: authors may deploy a workflow
  assuming compensation is handled, when in fact the default no-op is in place.
  Discovery only happens at runtime during an actual failure — exactly when correct
  rollback matters most.

## More Information

The supertrait relationship is conceptually aligned with Temporal's model, where a
workflow is always invocable as a regular function for its return value. It also
matches the Serverless Runtime's invocation model: compensation invocations are standard
invocations with a `CompensationContext` as params — they go through the same
`start_invocation` API path as forward calls, just with a different input shape.

The consequence that testing `compensate` in isolation requires a stub `call` impl
is an accepted trade-off. In practice, workflow structs carry shared state (API clients,
configuration) that is needed by both `call` and `compensate`; the two methods are
tested together on the same struct in integration tests, and separately via unit tests
where `call` is stubbed.

## Non-Applicable Domains

| Domain | Disposition | Reasoning |
|--------|-------------|-----------|
| PERF | N/A | Decision concerns trait hierarchy shape; vtable dispatch cost is negligible relative to handler I/O latency and is not a decision driver |
| REL | Addressed in Decision Outcome | Requiring a non-defaulted `compensate` ensures saga rollback is never silently absent — a direct reliability consequence of the chosen option |
| SEC | N/A | Decision concerns trait hierarchy; supertrait vs. independent trait has no authentication, authorization, or data protection implications |
| DATA | N/A | No data storage or schema involved in this decision |
| OPS | N/A | Pure library; no deployment topology, monitoring, or infrastructure concern |
| COMPL | N/A | Internal developer tooling; no regulatory requirement |
| UX | N/A | No end-user UI; developer ergonomics addressed in Decision Outcome and Pros/Cons |
| BIZ | N/A | Internal Rust library; no business stakeholder buy-in or cost analysis applicable to a trait hierarchy decision |

## Review Conditions

| Trigger | Action |
|---------|--------|
| A concrete requirement emerges for a "compensation-only" type (e.g., a registered function whose sole purpose is to roll back another function's side effects, with no forward call path) | Evaluate whether to introduce a separate `CompensationOnly` trait outside the `Handler` hierarchy, or to refine the definition of `WorkflowHandler` |
| The Serverless Runtime introduces a function category that does not invoke a forward `call` path before compensation (e.g., a pure saga-coordinator) | Re-evaluate Option B (independent trait) for that specific function category |
| Adapter registration patterns show that `dyn WorkflowHandler` and `dyn Handler` are never used interchangeably in practice | Reconsider supertrait coercion value; if never used, Option B may be simpler |

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

* `cpt-cf-serverless-sdk-core-fr-workflow-handler-trait` — `WorkflowHandler` trait structure and supertrait relationship
* `cpt-cf-serverless-sdk-core-component-workflow` — responsibility scope and boundaries of `workflow.rs`
* `cpt-cf-serverless-runtime-principle-unified-function` — SDK expression of the runtime's `workflow ⊂ function` model
* DESIGN.md §3.3 `WorkflowHandler<I, O> Trait Contract` — supertrait invariants and `compensate` idempotency requirement
