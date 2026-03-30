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

# ADR — `Context` as a Concrete Struct (Not a Trait or Generic Parameter)


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: Concrete struct `Context`](#option-a-concrete-struct-context)
  - [Option B: `trait InvocationContext`](#option-b-trait-invocationcontext)
  - [Option C: Generic parameter `Handler<I, O, C: InvocationContext>`](#option-c-generic-parameter-handleri-o-c-invocationcontext)
- [More Information](#more-information)
- [Non-Applicable Domains](#non-applicable-domains)
- [Review Conditions](#review-conditions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-serverless-sdk-core-adr-context-struct`

## Context and Problem Statement

`Handler::call` and `WorkflowHandler::compensate` receive an invocation context
that carries identity and execution-constraint fields (`invocation_id`, `tenant_id`,
`function_id`, `correlation_id`, `trace_id`, `span_id`, `function_version`,
`attempt_number`, `deadline`). Three structural approaches exist for expressing
this context in the trait signatures:

1. **Concrete struct** — a single `Context` struct with a fixed set of fields,
   passed as `&Context`.
2. **Trait** — an `InvocationContext` trait that adapters implement and handler authors
   receive as `&dyn InvocationContext` or `&impl InvocationContext`.
3. **Generic parameter** — a third type parameter on `Handler`, e.g.
   `Handler<I, O, C: InvocationContext>`, allowing adapters to supply any concrete type.

Which approach should the SDK use for the invocation context?

## Decision Drivers

* **[P1]** Handler implementations must be unit-testable without an adapter or runtime
  infrastructure — `Context` must be fully constructible in test code from plain values
  (`cpt-cf-serverless-sdk-core-nfr-testability`).
* **[P1]** The context fields passed to a handler are fully determined by the Serverless
  Runtime's `InvocationRecord`; the SDK projection is a fixed, documented mapping
  (DESIGN.md §3.1) — the set of fields is stable and platform-owned, not adapter-extensible.
* **[P2]** Adding a type parameter for context to `Handler<I, O>` widens the trait's
  generic surface, which propagates to all downstream trait bounds, adapter code, and
  type-erased `Box<dyn Handler<I, O>>` registries — complexity cost must be justified.
* **[P2]** The crate follows the principle of minimal trusted surface
  (`cpt-cf-serverless-sdk-core-principle-minimal-surface`): no type or abstraction is
  added unless directly required by a stated requirement.
* **[P3]** Adapter authors must be able to construct `Context` to drive handler invocations;
  the construction path must be straightforward and not require implementing a trait.

## Considered Options

* **Option A**: Concrete struct `Context` — a single struct with all fields as public
  members; passed as `&Context` in both `call` and `compensate`.
* **Option B**: `trait InvocationContext` — an abstract trait with accessor methods for
  each field; adapters implement the trait, handlers receive `&dyn InvocationContext`.
* **Option C**: Generic parameter `Handler<I, O, C: InvocationContext>` — `Context` is a
  generic type parameter on the `Handler` trait, bounded by an `InvocationContext` trait;
  adapters supply a concrete `C` and handlers are generic over it.

## Decision Outcome

Chosen option: **Option A: Concrete struct `Context`**, because the set of context fields
is fully specified by the Serverless Runtime's `InvocationRecord` mapping and is
platform-owned — there is no legitimate scenario where an adapter needs to supply a
different context shape, only a different set of field *values*. A concrete struct is
directly constructible in tests without any runtime infrastructure, which is essential
for handler unit testing. Adding a third generic parameter to `Handler` or introducing a
trait for a fixed, non-polymorphic data bag would add complexity with no corresponding
benefit, violating the minimal-surface principle.

### Consequences

* `Context` has a fixed set of 9 fields. Adapter authors populate these fields from the
  runtime's `InvocationRecord` before calling the handler; the field mapping is documented
  in DESIGN.md §3.1 and enforced by adapters, not by the SDK's type system.
* Adding a new context field (e.g., a future `security_principal` field from the runtime)
  is a compile-breaking change for every adapter that constructs `Context` via struct
  literal syntax. `Context` is intentionally **not** `#[non_exhaustive]`: the compile
  error at adapter construction sites is the intended mechanism for keeping adapter-side
  `InvocationRecord → Context` mappings in sync with the field set. The breakage scope
  is limited and accepted given the platform-owned, stable nature of `InvocationRecord`.
* Adapters cannot extend `Context` with adapter-specific fields (e.g., a Temporal
  workflow run ID). Such fields must be passed through other mechanisms (e.g., a
  separate adapter-specific context struct passed alongside `Context`, or accessed
  via `env: &dyn Environment`).
* Handler tests construct `Context` directly with plain literals — no mock framework
  or trait-implementing stub needed. This is the primary testability benefit.

### Confirmation

* `Handler::call` and `WorkflowHandler::compensate` signatures use `&Context`, not
  `&dyn InvocationContext` or a generic `C` — verified by inspection of `handler.rs`
  and `workflow.rs`.
* `Context` is constructible in `#[cfg(test)]` blocks using struct literal syntax —
  confirmed by the existence of unit tests that build `Context` from plain field values.
* No `InvocationContext` trait exists in the crate — verified by `cargo doc --no-deps`.

## Pros and Cons of the Options

### Option A: Concrete struct `Context`

A single `Context` struct with public fields; `Handler::call(&self, ctx: &Context, ...)`.

* Good, because fully constructible in unit tests with no infrastructure.
* Good, because field access is direct (`ctx.tenant_id`, not `ctx.tenant_id()`) —
  no boilerplate accessor calls.
* Good, because no additional generic parameter on `Handler` — adapter type-erased
  registries (`Box<dyn Handler<I, O>>`) work without extra bounds.
* Good, because the platform-owned field set is expressed once, in one place, with
  one type, owned by the SDK.
* Neutral, because adapters cannot add fields — adapter-specific data must travel
  through other channels. Acceptable given the adapter-agnostic principle.
* Bad, because adding a context field breaks all adapter struct literal construction
  of `Context` — a compile-visible change at every adapter construction site. This
  breakage is intentional (it forces mapping updates) and mitigated by the
  platform-owned, stable nature of `InvocationRecord`.

### Option B: `trait InvocationContext`

An abstract trait; adapters implement it; handlers receive `&dyn InvocationContext`.

* Good, because adapters can add adapter-specific fields by extending the trait or
  providing a wider concrete type.
* Good, because the SDK is not tied to a specific set of fields — the trait can be
  extended without a struct layout change.
* Bad, because constructing a `dyn InvocationContext` in a unit test requires either
  a mock struct implementing the trait or a test double — more test setup boilerplate.
* Bad, because a trait over a fixed data bag implies behavioral polymorphism where
  none exists — `InvocationRecord` has a well-specified, non-varying shape; the
  trait abstraction signals the wrong design intent.
* Bad, because the trait becomes part of the public API surface that adapter authors
  must implement correctly — more surface area to document, test, and maintain.
* Bad, because it implies that the context is behaviorally polymorphic, which it is
  not — `InvocationRecord` has a fixed, well-specified shape.

### Option C: Generic parameter `Handler<I, O, C: InvocationContext>`

`Handler` gains a third generic parameter `C`; adapters supply their concrete type.

* Good, because zero-cost abstraction — no vtable, no runtime dispatch.
* Good, because adapters can carry adapter-specific state in their `C` type without
  any SDK change.
* Bad, because every use of `Handler` in adapter code, type registries, and wrapper
  types now carries three type parameters instead of two — significant complexity
  propagation.
* Bad, because `Box<dyn Handler<I, O, C>>` is not object-safe for varying `C` —
  adapters that store type-erased handlers must pin `C` to a concrete type, eliminating
  the polymorphism benefit.
* Bad, because it violates the minimal-surface principle: a third generic is added
  for adapter extensibility that is not required by any current or planned requirement.
* Bad, because the `InvocationContext` trait must still be defined and documented
  even though the platform's context is fully specified and does not need to vary.

## More Information

The `Context` field set is a projection of the Serverless Runtime's `InvocationRecord`
schema (DESIGN.md §3.1, `InvocationRecord → Context Field Mapping`). The runtime's
`InvocationRecord` is a GTS-typed schema; the SDK has no control over its evolution.
When the runtime adds a new field to `InvocationRecord` that is relevant to handler
authors, it will be added to `Context` in a semver-compatible manner.

The decision is analogous to AWS Lambda's `lambda_runtime::Context` and Temporal's
`workflow.Info()` — both are concrete data types, not traits, because the runtime
unambiguously owns the invocation identity fields.

## Non-Applicable Domains

| Domain | Disposition | Reasoning |
|--------|-------------|-----------|
| PERF | N/A | Decision concerns struct vs. trait shape for a per-invocation data bag; field access cost is negligible relative to handler I/O latency and is not a decision driver |
| SEC | N/A | Decision concerns struct vs. trait ergonomics; `Context` carries identity metadata, not credentials or secrets — no authentication/authorization concern at this layer |
| REL | N/A | No stateful data or availability SLO; library type definition only |
| DATA | N/A | `Context` is an in-memory, per-invocation data bag; no persistence, no schema ownership |
| OPS | N/A | Pure library; no deployment topology, monitoring, or infrastructure concern |
| COMPL | N/A | Internal developer tooling; no regulatory requirement |
| UX | N/A | No end-user UI; developer ergonomics addressed in Decision Outcome and Pros/Cons |
| BIZ | N/A | Internal Rust library; no business stakeholder buy-in or cost analysis applicable |

## Review Conditions

| Trigger | Action |
|---------|--------|
| The Serverless Runtime introduces a new adapter-specific context field that must be propagated to handler authors (e.g., workflow run ID, parent span) | Evaluate whether to add the field to `Context` (Option A), introduce a companion `AdapterContext` type, or revisit Option B |
| A handler use case emerges that requires context polymorphism (e.g., a handler that works in both serverless and non-serverless contexts) | Re-evaluate Option B or Option C |
| The `InvocationRecord` schema changes structurally (field removal or rename) | Assess breaking-change impact; update `Context` with a semver bump if required |

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

* `cpt-cf-serverless-sdk-core-fr-context` — `Context` struct definition and field set
* `cpt-cf-serverless-sdk-core-fr-deadline-helpers` — `is_deadline_exceeded()` and `remaining_time()` helpers on the concrete struct
* `cpt-cf-serverless-sdk-core-principle-minimal-surface` — no trait or generic added without a stated requirement
* `cpt-cf-serverless-sdk-core-principle-impl-agnostic` — `Context` carries no adapter-specific type
* `cpt-cf-serverless-sdk-core-component-context` — responsibility scope and boundaries of `context.rs`
