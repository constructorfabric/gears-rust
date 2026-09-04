---
status: accepted
date: 2026-05-18
---

# ADR-0004: `WorkflowHandler` as Supertrait of `FunctionHandler`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
  - [Operating Envelope](#operating-envelope)
  - [Applicability](#applicability)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Supertrait `WorkflowHandler: FunctionHandler`](#supertrait-workflowhandler-functionhandler)
  - [Co-trait](#co-trait)
  - [Capability marker](#capability-marker)
- [More Information](#more-information)
  - [Related Decisions](#related-decisions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-serverless-runtime-plugin-sdk-adr-workflow-supertrait`

## Context and Problem Statement

The SDK needs to express the relationship between two authoring shapes: a stateless function (one call, one result) and a durable workflow (a function whose execution can be compensated on failure or cancellation). The relationship can be modelled as a supertrait, as two co-equal traits, or as a capability marker. The chosen shape determines how compensation surfaces in adapter code and in the conformance suite.

## Decision Drivers

- A workflow's primary path is identical to a function's: typed input, typed output, `Context`, `Environment`, `ServerlessSdkError`. Compensation is additive, not separate.
- Adapter authors should progress from "I implement a function" to "I also implement compensation" without re-shaping the type.
- The conformance suite tests the invocation path once; a workflow's invocation path should reuse the function-handler fixtures.
- Host dispatch (`RuntimeAdapter::execute`) is invocation-shaped, not workflow-shaped — the workflow-ness of a handler lives inside the adapter, not at the host boundary.

## Considered Options

- Supertrait: `WorkflowHandler<I, O>: FunctionHandler<I, O>` plus a `compensate(…)` method.
- Co-trait: `FunctionHandler` and `WorkflowHandler` declared independently, each with its own invocation method.
- Capability marker: `FunctionHandler` plus a `Compensable` marker trait carrying the compensation method.

## Decision Outcome

Chosen: **supertrait `WorkflowHandler<I, O>: FunctionHandler<I, O>`**, because it expresses the additive nature of compensation directly and lets the conformance suite reuse the function-handler invocation path for the workflow's primary execution.

### Consequences

- A workflow adapter implementor implements `FunctionHandler::call` for the primary path and `WorkflowHandler::compensate` for the rollback path; both live on the same handler value or on a thin wrapper the adapter chooses.
- A workflow's `<I, O>` is constrained to the same shape as its function-handler supertrait. Handler authors must therefore design `I` to make sense as a function invocation input (the primary path consumes it) even when the workflow's natural framing might be different. This is the deliberate cost of reusing the function-handler lens.
- `compensate` takes a structured `CompensationInput` (covered by ADR-0003's `#[non_exhaustive]` discipline), not the same `I` as `call`. Compensation observes only the side-effects accumulated by partial steps, not the original input; `CompensationInput` carries that observed state, decoupled from `I`. The runtime adapter is the only constructor of `CompensationInput` values; handlers never construct one.
- Generic helpers (`trace::call_instrumented`, `trace::compensate_instrumented`) can be defined over the function-handler bound and reuse the same `Context` / `Environment` plumbing.
- Adapters that do not need compensation implement only `FunctionHandler`; they pay no compensation cost.

### Confirmation

- The supertrait relationship is enforced by the type system: an adapter that implements `WorkflowHandler` without also implementing `FunctionHandler<I, O>` is a compile error.
- The conformance suite exercises both `call` and `compensate` against the same handler value and confirms that the call path behaves identically to the function-only conformance run.

### Operating Envelope

The supertrait shape is sound under the following assumption; outside it, a co-trait or split-trait alternative becomes the better fit.

- **Workflow `<I, O>` fits the function-handler lens.** The primary execution of a workflow consumes the same `I` and produces the same `O` as a stateless function. If a future workflow type needs fundamentally different I/O semantics from any function-handler shape — for example, `I` as a workflow definition and `O` as a workflow handle — the supertrait becomes a straitjacket and this ADR is a revisit candidate.
- **One additive capability only.** If a second workflow capability beyond compensation lands on `WorkflowHandler` (signals, queries, replay primitives, child-workflow orchestration), the supertrait becomes a kitchen sink. At that point, composition — keeping `FunctionHandler` minimal and defining each capability as its own small trait the adapter holds alongside the handler — is the better shape, and this ADR is a revisit candidate.

### Applicability

| Domain | Status | Notes |
|---|---|---|
| ARCH | Addressed | Supertrait shape; compensation additive. |
| INT | Addressed | One trait surface for adapters that need compensation; function-handler adapters are untouched. |
| MAINT | Addressed | Conformance and trace helpers reuse the function-handler bound. |
| TEST | Addressed | Conformance suite exercises both `call` and `compensate` against one handler value. |
| REL | N/A | No runtime state owned by the trait declaration. |
| PERF | N/A | Zero runtime cost — supertrait composition is compile-time only. |
| SEC | N/A | No trust boundary introduced. |
| DATA | N/A | No persistence; durable-state semantics live in the runtime adapter. |
| OPS | N/A | No deployable surface introduced. |
| COMPL / UX / BIZ | N/A | Internal SDK; no end-user or regulated surface. |

## Pros and Cons of the Options

### Supertrait `WorkflowHandler: FunctionHandler`

- Good, because compensation is modelled as an addition, not a parallel hierarchy.
- Good, because conformance and trace helpers built for `FunctionHandler` apply unchanged.
- Good, because adapters that need only the function path are not forced through workflow scaffolding.
- Neutral, because `I` and `O` must match across `call` and `compensate`'s primary invocation; compensation receives a separate `CompensationInput`, which is the natural shape.
- Bad, because adding a second additive capability beyond compensation (signals, replay, child workflows) would pile onto `WorkflowHandler` and turn it into a kitchen-sink trait; composition becomes the better shape at that point (see Operating Envelope).

### Co-trait

- Good, because compensation can have a fully independent signature.
- Bad, because the invocation path is duplicated across both traits.
- Bad, because conformance fixtures must branch on which trait is implemented; testing the function path for a workflow handler then requires distinct fixture paths.

### Capability marker

- Good, because the function trait stays minimal and compensation opts in via a marker.
- Bad, because compensation lives in a marker trait detached from the function-handler value: the host or adapter must call the marker's method through a separate path, splitting the call site that a supertrait keeps unified.

## More Information

### Related Decisions

- **ADR-0001** (async-trait everywhere) — both `call` and `compensate` are async trait methods governed by this discipline; the `#[async_trait]` annotation lands on the supertrait declaration.
- **ADR-0003** (`#[non_exhaustive]` surface) — `CompensationInput` is a `#[non_exhaustive]` struct with a documented public construction path; future fields can be added without breaking adapters.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

- `cpt-cf-serverless-runtime-plugin-sdk-fr-workflow-handler-trait` — supertrait shape.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-compensation-input` — structured `CompensationInput` consumed by `compensate`.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-handler-trait` — function-handler base trait.
- `cpt-cf-serverless-runtime-plugin-sdk-component-workflow` — `workflow.rs` houses the trait and `CompensationInput`.
- `cpt-cf-serverless-runtime-plugin-sdk-interface-workflow-trait` — interface contract.
- `cpt-cf-serverless-runtime-plugin-sdk-seq-compensate` — compensation sequence in DESIGN §3.6.
