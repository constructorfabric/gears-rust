---
status: proposed
date: 2026-03-23
owner: SDK architecture team
scope: modules/serverless-sdk
priority: p2
---

# ADR — CompensationInput as a Structured Type (Not a Generic Handler Input)




<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: Structured `CompensationInput`](#option-a-structured-compensationinput)
  - [Option B: Generic input via `I`](#option-b-generic-input-via-i)
  - [Option C: Separate `CompensationHandler<C>` trait](#option-c-separate-compensationhandlerc-trait)
- [More Information](#more-information)
- [Non-Applicable Domains](#non-applicable-domains)
- [Review Conditions](#review-conditions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-serverless-sdk-core-adr-compensation-input`

## Context and Problem Statement

`WorkflowHandler<I, O>` extends `Handler<I, O>` with a `compensate` method. The runtime's
`CompensationContext` (`gts.x.core.serverless.compensation_context.v1~`) is passed to the
compensation function as its `params` field — the same JSON slot used for normal handler
input. This means two valid approaches exist for the `compensate` method signature:

1. **Structured input**: `compensate` receives a dedicated `CompensationInput` struct
  populated by the adapter from the `CompensationContext` JSON.
2. **Generic input**: `compensate` receives the same generic `I` type as `call`, requiring
  the function author to declare their compensation handler's input type as a
   `CompensationContext`-compatible struct.

Which approach should the `compensate` method use?

## Decision Drivers

- **[P1]** Compensation is a distinct platform concept (saga rollback), not a normal
  function invocation — its input carries different fields (`trigger`,
  `original_invocation_id`, `failed_step_id`, `workflow_state_snapshot`, etc.) than a
  business function's input `I` — the type system must express this distinction; mixing
  the two is a correctness hazard.
- **[P2]** The adapter must construct `CompensationInput` from the runtime's JSON envelope
  regardless; the question is whether that struct is defined in this crate or by the
  function author — the platform owns the envelope's shape, so the SDK should own the type.
- **[P2]** Function authors should not need to re-declare a `CompensationContext`-compatible
  struct to implement compensation — reduces boilerplate and eliminates the risk of
  incompatible author-defined structs.
- **[P3]** Compensation idempotency guidance (check `original_invocation_id` before any
  side effect) is invariant across all handlers; it should be expressible in shared SDK
  documentation, not scattered across individual handler structs — documentation and
  discoverability concern.

## Considered Options

- **Option A**: Structured `CompensationInput` type — the SDK defines a concrete
`CompensationInput` struct with all `CompensationContext` fields; `compensate` receives it.
- **Option B**: Generic input via `I` — `compensate` receives the same generic `I` as `call`;
the function author defines `I` to be deserializable from `CompensationContext`.
- **Option C**: Separate `CompensationHandler<C>` trait — compensation is a standalone trait
independent of `WorkflowHandler`; `C` is the compensation input type chosen by the author.

## Decision Outcome

Chosen option: **Option A: Structured `CompensationInput` type**, because compensation is
a platform-owned, well-specified operation whose input envelope is defined by the runtime's
`CompensationContext` schema. Defining `CompensationInput` in the SDK crate ensures every
compensation handler receives an identical, documented struct with guaranteed field presence,
makes the idempotency contract (check `original_invocation_id`) discoverable at the type
level, and eliminates the risk of authors inadvertently declaring an incompatible input type.

Option B was rejected because making `compensate` receive generic `I` forces authors to
couple their business input type to the `CompensationContext` schema; incompatibility is
only detectable at runtime via JSON deserialization failure, not at compile time.

Option C was rejected because a standalone `CompensationHandler<C>` trait breaks the
supertrait relationship established in `cpt-cf-serverless-sdk-core-adr-workflow-supertrait`,
removing the compile-time guarantee that every workflow is also a callable function.

### Consequences

- `WorkflowHandler<I, O>::compensate` receives `CompensationInput`, not `I`. This means the
same struct type handles all workflow compensation, regardless of the workflow's business
input type.
- `CompensationInput` is `#[non_exhaustive]`, so future fields from the runtime's
`CompensationContext` schema can be added without breaking existing compensation handlers.
- The adapter is responsible for deserialising the runtime's `CompensationContext` JSON into
`CompensationInput` before calling `compensate`. The mapping is documented in `DESIGN.md §3.1`.
- A workflow function that serves as its own compensation handler (where `I` happens to be
compatible with `CompensationContext`) cannot reuse `call` for `compensate`; it must
implement both methods separately. This is intentional: the two operations have different
semantics and should not share an implementation path.

### Confirmation

- `WorkflowHandler::compensate` signature in `workflow.rs` uses `CompensationInput`, not `I`.
- `CompensationInput` is declared `#[non_exhaustive]` — verified by code inspection.
- `CompensationTrigger` is declared `#[non_exhaustive]` — verified by code inspection.
- The DESIGN.md `CompensationContext → CompensationInput` mapping table covers all fields
from the runtime's `CompensationContext` schema.

## Pros and Cons of the Options

### Option A: Structured `CompensationInput`

The SDK defines `CompensationInput` to match the runtime's `CompensationContext` envelope.

- Good, because the compensation input type is owned by the platform — function authors
cannot accidentally define an incompatible struct.
- Good, because all compensation-relevant fields (`trigger`, `original_invocation_id`,
`workflow_state_snapshot`, etc.) are always present and named consistently.
- Good, because SDK documentation for `compensate` can reference specific fields by name,
making the idempotency and rollback-scope guidance concrete.
- Good, because `CompensationInput` is shared across all workflow handlers — one place to
update when the runtime's `CompensationContext` schema evolves.
- Neutral, because the function author has no control over compensation input type — some
may prefer to define their own. Acceptable: the platform owns the envelope.
- Bad, because a workflow whose business input happens to be `CompensationContext`-shaped
cannot unify `call` and `compensate` behind a single `I`. Edge case; not a realistic scenario.

### Option B: Generic input via `I`

`compensate` takes the same `I` as `call`; authors declare `I` to be compensation-compatible.

- Good, because no additional type is introduced; the existing `I` generic is reused.
- Good, because a handler that processes compensation events as ordinary invocations can
unify both paths.
- Bad, because the platform cannot guarantee that `I` deserialises from `CompensationContext`.
Authors may define an `I` that is missing required fields (`trigger`, `original_invocation_id`),
leading to silent data loss or runtime deserialization errors.
- Bad, because SDK documentation cannot reference specific compensation fields by name;
guidance for idempotency and rollback scope must be entirely in prose.
- Bad, because it creates an implicit coupling: the function's `IOSchema.params` must be
compatible with `CompensationContext`, which is a platform schema — effectively forcing
the business input type to be shaped like a compensation envelope.

### Option C: Separate `CompensationHandler<C>` trait

Compensation is a standalone trait with a generic input `C` chosen by the author.

- Good, because compensation and normal invocation are fully independent; different `I` and `C` types.
- Good, because the compensation input type can be versioned separately from `I`.
- Bad, because it severs the explicit relationship between `WorkflowHandler` and compensation;
adapters must check for two separate trait implementations.
- Bad, because function registration must handle both `Handler<I, O>` and
`CompensationHandler<C>` independently, complicating adapter discovery.
- Bad, because the platform already owns the `CompensationContext` schema, so an
author-controlled `C` provides no independent value while adding a third generic parameter
to every `WorkflowHandler` implementor.

## More Information

The Serverless Runtime design specifies that compensation functions are "regular functions
invoked via the standard invocation flow" with `CompensationContext` as `params`
(DESIGN.md §3.1, WorkflowTraits). Option A aligns the SDK with this design: the same
invocation flow applies, and `CompensationInput` is the SDK-level projection of the
platform's `CompensationContext` schema.

## Non-Applicable Domains

The following checklist domains are not applicable to this ADR and are explicitly excluded:


| Domain | Disposition | Reasoning                                                                                                                                                                   |
| ------ | ----------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| PERF   | N/A         | `CompensationInput` is a simple deserialization target; the per-invocation cost of deserialising a small JSON struct is negligible compared to the network I/O of a compensation call. Compensation is a low-frequency event (saga rollback on failure), not a hot inner loop. |
| SEC    | N/A         | Decision concerns compensation input type design; `CompensationInput` carries saga-rollback metadata, not credentials or user PII                                           |
| REL    | N/A         | No stateful data or SLO; library trait definition only                                                                                                                      |
| DATA   | N/A         | `CompensationInput` is a deserialization target (no persistence, no schema ownership in this crate); the runtime's `CompensationContext` schema is the authoritative source |
| OPS    | N/A         | Pure library; no deployment topology, monitoring, or operational concern                                                                                                    |
| COMPL  | N/A         | Internal developer tooling; no regulatory, certification, or legal requirement                                                                                              |
| UX     | N/A         | No end-user UI; developer ergonomics are addressed in Decision Outcome                                                                                                      |
| BIZ    | N/A         | Internal Rust library crate; no business stakeholder buy-in, cost analysis, or time-to-market consideration applicable to a trait signature decision                        |


## Review Conditions

This ADR should be revisited when any of the following conditions is met:


| Trigger                                                                                                                 | Action                                                                                                          |
| ----------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| The Serverless Runtime's `CompensationContext` schema adds new top-level fields                                         | Review `CompensationInput` field set and update struct; additions are backward-safe given `#[non_exhaustive]` |
| The Serverless Runtime's `CompensationContext` schema removes or renames an existing field                              | Assess breaking-change impact on all `CompensationInput` consumers; `#[non_exhaustive]` does NOT protect against removals/renames — SDK semver bump required |
| A concrete requirement emerges for a workflow handler that needs different compensation input types per handler variant | Re-evaluate Option C (`CompensationHandler<C>` trait)                                                           |
| The runtime introduces a second compensation envelope type (e.g., partial vs. full rollback)                            | Evaluate whether `CompensationInput` can be extended or whether a second type is needed                         |


## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

- `cpt-cf-serverless-sdk-core-fr-workflow-handler-trait` — `compensate` method signature
- `cpt-cf-serverless-sdk-core-fr-compensation-input` — `CompensationInput` struct definition
- `cpt-cf-serverless-sdk-core-component-workflow` — responsibility scope of `workflow.rs`

