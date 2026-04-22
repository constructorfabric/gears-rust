---
status: accepted
date: 2026-03-23
---
<!--
 =============================================================================
 ARCHITECTURE DECISION RECORD (ADR) — based on MADR format
 =============================================================================
 PURPOSE: Capture WHY Function and Workflow were chosen as sibling peer base
 types rather than a parent/child (Function → Workflow) or abstract-base
 (Entrypoint/Callable → Function | Workflow) hierarchy.

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
# ADR — Function | Workflow as Sibling Peer Base Types


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option D: Function | Workflow (sibling peer types) — **chosen**](#option-d-function--workflow-sibling-peer-types--chosen)
  - [Option A: Function → Workflow (function as base type) — rejected](#option-a-function--workflow-function-as-base-type--rejected)
  - [Option B: Entrypoint → Function | Workflow (abstract entrypoint base) — rejected](#option-b-entrypoint--function--workflow-abstract-entrypoint-base--rejected)
  - [Option C: Callable → Function | Workflow (abstract callable base) — rejected](#option-c-callable--function--workflow-abstract-callable-base--rejected)
- [More Information](#more-information)
  - [Code-Level Callable Resolution in ModKit](#code-level-callable-resolution-in-modkit)
  - [GTS Schema Callable References](#gts-schema-callable-references)
- [Traceability](#traceability)

<!-- /toc -->

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: Function → Workflow (function as base type)](#option-a-function--workflow-function-as-base-type)
  - [Option B: Entrypoint → Function | Workflow (abstract entrypoint base)](#option-b-entrypoint--function--workflow-abstract-entrypoint-base)
  - [Option C: Callable → Function | Workflow (abstract callable base)](#option-c-callable--function--workflow-abstract-callable-base)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-serverless-runtime-adr-callable-type-hierarchy`

## Context and Problem Statement

The Serverless Runtime domain model needs a GTS type hierarchy for callable entities — functions and workflows. The original proposal used an abstract `Entrypoint` base type with `Function` and `Workflow` as siblings (`entrypoint → function | workflow`). A subsequent proposal adopted `function → workflow` (Function as base, Workflow as derived type). This ADR addresses the final decision: whether to retain Function as the parent of Workflow, or to model them as independent sibling peer types.

A function, in the general sense, is something that accepts inputs, produces outputs, and may have side effects. Both plain functions and workflows share these fundamental characteristics — they are defined with input/output schemas, invoked with parameters, and return results. Workflows add specific capabilities on top (durable state, compensation, event waiting). The question is whether this "is a superset of" relationship should be encoded as GTS type inheritance, or whether the shared base semantics are better expressed through identical field contracts on two independent types.

## Decision Drivers

* Shared fields (schema, limits, retry, rate limit, implementation, lifecycle_status) are identical on both functions and workflows — neither type is the conceptual "owner" of these fields
* GTS type inheritance implies substitutability: a runtime that accepts `function.v1~` would match workflows if workflow derived from function, causing adapters that only support plain functions to accidentally receive workflows
* Function-only runtimes need clean positive matching on `function.v1~` without needing to negatively exclude `workflow.v1~` derived types
* Workflow-only runtimes similarly need clean positive matching on `workflow.v1~`
* The model must remain runtime-agnostic — no single executor implementation should dictate the type system
* Workflow-specific traits (checkpointing, compensation, event waiting) are additive capabilities not shared with functions — they do not belong on a shared base

## Considered Options

* **Option A**: Function → Workflow (function as base type, workflow derived)
* **Option B**: Entrypoint → Function | Workflow (abstract entrypoint base)
* **Option C**: Callable → Function | Workflow (abstract callable base)
* **Option D**: Function | Workflow (sibling peer types — independent base types)

## Decision Outcome

Chosen option: **"Option D: Function | Workflow (sibling peer types)"**, because identical base semantics make siblings cleaner at the GTS level — a workflow is conceptually a superset of a function but GTS type inheritance does not need to encode this; the sibling model avoids function-only runtimes accidentally matching workflows via type inheritance; and each type can be matched positively and independently without negative exclusions.

### Consequences

* Functions and workflows are independent GTS base types: `gts.x.core.sless.function.v1~` and `gts.x.core.sless.workflow.v1~`
* Both types carry identical base fields (version, tenant_id, owner, status, schema, traits: invocation/limits/retry/rate_limit, implementation) — the shared contract is enforced by schema convention, not GTS inheritance
* Runtimes that support only functions match positively on `gts.x.core.sless.function.v1~*` with no need to exclude workflow subtypes
* Runtimes that support only workflows match positively on `gts.x.core.sless.workflow.v1~*`
* References to "any callable" (schedules, triggers, invocation records) carry a comment noting both base types; code-level abstractions may use a union or protocol/interface
* The `workflow_traits` block (checkpointing, compensation, suspension) lives exclusively on the workflow type and is not inherited by functions
* No abstract base type exists that is never instantiated, eliminating phantom types

### Confirmation

* DESIGN_GTS_SCHEMAS.md defines `gts.x.core.sless.function.v1~` and `gts.x.core.sless.workflow.v1~` as independent schemas with no `allOf`/`$ref` relationship between them
* Both schemas carry identical base fields by convention
* Adapter/runtime capability routing uses positive GTS type matching on `function.v1~*` or `workflow.v1~*` independently
* Code review verifies that SDK types treat Function and Workflow as peer structs sharing a common interface/protocol, not as parent/child structs

## Pros and Cons of the Options

### Option D: Function | Workflow (sibling peer types) — **chosen**

Function and Workflow are independent base types. Both carry the same base fields by schema convention. Neither derives from the other.

**GTS hierarchy:**
```
gts.x.core.sless.function.v1~    ← function base (independent)
gts.x.core.sless.workflow.v1~    ← workflow base (independent sibling — NOT derived from function)
```

| | Aspect | Note |
|---|--------|------|
| Pro | Function-only runtimes match positively on `function.v1~*` | No negative matching needed to exclude workflows |
| Pro | Workflow-only runtimes match positively on `workflow.v1~*` | Clean, unambiguous routing |
| Pro | Identical base semantics don't require inheritance to express | Schema convention is sufficient; GTS inheritance is reserved for genuine specialisation |
| Pro | No accidental substitutability across type boundaries | A function-only runtime cannot accidentally receive a workflow |
| Pro | Each type evolves independently | Adding fields to workflow doesn't affect function consumers |
| Neutral | "Any callable" references require noting both types | Schedules, triggers, and invocation records carry a comment that both `function.v1~*` and `workflow.v1~*` are valid; no single wildcard captures both |
| Con | Base field duplication between schemas | Shared fields are duplicated by convention rather than being inherited — mitigated by tooling that validates both schemas against the same field contract |

### Option A: Function → Workflow (function as base type) — rejected

Function is the base callable type. Workflow extends Function with additional traits for durable execution.

**GTS hierarchy:**
```
gts.x.core.sless.function.v1~                                          — base
gts.x.core.sless.function.v1~x.core.sless.workflow.v1~                 — derived
```

| | Aspect | Note |
|---|--------|------|
| Pro | "Function" accurately describes all callables | They accept inputs, produce outputs, and may have side effects |
| Pro | Two types instead of three | Simpler cognitive model and fewer abstractions |
| Pro | All callable roles share the base type naturally | Helpers, abstract bases, and direct targets are all functions |
| Pro | A workflow **is** a function | It simply adds durable execution capabilities on top |
| Pro | Shared fields live on Function | The type that is actually instantiated, not a phantom abstract base |
| Neutral | Adapters wanting only plain functions need negative matching | Must exclude `workflow.v1~` derived types rather than positive-match a dedicated type |
| Con | Function-only runtimes accidentally match workflows via inheritance | GTS type matching on `function.v1~` would include all derived workflow types — requires explicit exclusion logic |
| Con | "All workflows are functions" may initially surprise | Though the shared input→output nature makes the relationship accurate |

### Option B: Entrypoint → Function | Workflow (abstract entrypoint base) — rejected

An abstract Entrypoint type carries shared fields. Function and Workflow are sibling derived types.

**GTS hierarchy:**
```
gts.x.core.sless.entrypoint.v1~                                         — abstract base
gts.x.core.sless.entrypoint.v1~x.core.sless.function.v1~               — derived
gts.x.core.sless.entrypoint.v1~x.core.sless.workflow.v1~               — derived
```

| | Aspect | Note |
|---|--------|------|
| Pro | Function and Workflow evolve independently | Sibling types at the type level |
| Pro | Positive GTS type matching for runtimes | Function-only and workflow-only runtimes are first-class citizens |
| Con | "Entrypoint" names the base after a usage pattern | Helpers, abstract bases, and utilities are not "entrypoints" but must derive from this type |
| Con | Not every callable is an entry point | The name misrepresents what the entity is |
| Con | Abstract base is never instantiated directly | Phantom type exists only to carry shared fields |
| Con | Three types instead of two | Increased complexity without proportional benefit |

### Option C: Callable → Function | Workflow (abstract callable base) — rejected

Same structure as Option B but with a neutral name that avoids the "entrypoint" naming issue.

**GTS hierarchy:**
```
gts.x.core.sless.callable.v1~                                           — abstract base
gts.x.core.sless.callable.v1~x.core.sless.function.v1~                 — derived
gts.x.core.sless.callable.v1~x.core.sless.workflow.v1~                 — derived
```

| | Aspect | Note |
|---|--------|------|
| Pro | "Callable" is a neutral name | Doesn't imply a specific usage pattern |
| Pro | Function and Workflow are explicit sibling types | Independent evolution |
| Con | "Callable" is a synonym for "function" | The distinction adds a type without adding meaning |
| Con | Three types instead of two | Abstract base is never instantiated directly |
| Con | Shared fields live on a type never used directly | Creates indirection for implementers |

## More Information

The previous project's `Entrypoint` type was introduced when all callables were assumed to be top-level invocation targets. As the domain evolved to include helper functions, abstract base definitions for runtime extension, and utility functions that are called by other functions (not directly invoked by users), the "entrypoint" name became misleading.

The intermediate `function → workflow` model (Option A) was a reasonable step: a workflow is genuinely a superset of a function at the semantic level. However, encoding this at the GTS type level creates a routing hazard: any runtime registered to handle `gts.x.core.sless.function.v1~` would, by GTS inheritance rules, also match workflow instances unless it explicitly excluded them. The sibling model eliminates this hazard entirely. The "conceptually a superset" relationship is documented here and in the schema descriptions, but not encoded as GTS inheritance.

The execution mode (sync vs async) and invocation role (direct target vs helper vs abstract base) are orthogonal to type identity and are handled via invocation parameters and runtime configuration, not via the GTS type hierarchy.

### Code-Level Callable Resolution in ModKit

The sibling model has a direct implication for how "any callable" references work at the code level in ModKit. `ClientHub` resolves dependencies by trait type, not by GTS type — so there is no single `hub.get::<dyn CallableClient>()` that returns both functions and workflows.

**Chosen approach:** The SDK defines a single `ServerlessRuntimeClient` trait (see [DESIGN.md, section 1.4.3](../DESIGN.md#143-sdk-crate)) whose methods accept callable references by GTS ID string. The client implementation resolves internally whether the ID refers to a function or workflow by inspecting the GTS type prefix (`function.v1~` vs `workflow.v1~`). This avoids requiring callers to make two separate `hub.get_scoped::<dyn ...>()` calls or to know the callable type in advance.

```rust
// Caller does not need to know if the target is a function or workflow:
let sless = hub.get::<dyn ServerlessRuntimeClient>()?;
let record = sless.invoke(ctx, InvokeRequest {
    function_id: "gts.x.core.sless.workflow.v1~vendor.app.orders.process_order.v1~".into(),
    mode: InvocationMode::Async,
    params: serde_json::json!({ "order_id": "ORD-123" }),
    ..Default::default()
}).await?;
```

**Rejected alternative:** A union `CallableClient` trait wrapping separate `FunctionClient` and `WorkflowClient` traits was considered but rejected because it would force all callers to choose a branch at call time, negating the benefit of the unified invocation surface.

### GTS Schema Callable References

Schedule, Trigger, and Webhook Trigger schemas use `function_id` with `x-gts-ref: gts.x.core.sless.function.v1~*`. The description text notes that workflows use `workflow.v1~*`, but the `x-gts-ref` constraint only matches functions. This is a known inconsistency addressed as follows:

- The `function_id` field is renamed to `callable_id` in the Rust SDK models (`models.rs`) to accurately reflect that it accepts both types.
- The GTS schemas retain `function_id` for backward compatibility but add a `callable_type` discriminator field:

```json
{
  "callable_type": {
    "type": "string",
    "enum": ["function", "workflow"],
    "description": "Type of the referenced callable. Determines which GTS type family the callable_id belongs to."
  },
  "function_id": {
    "type": "string",
    "description": "GTS ID of the callable. Must match gts.x.core.sless.function.v1~* when callable_type is 'function', or gts.x.core.sless.workflow.v1~* when callable_type is 'workflow'."
  }
}
```

- Validation logic checks that `function_id` matches the correct GTS type pattern based on `callable_type`. If `callable_type` is absent, the runtime infers it from the GTS type prefix.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

* [DESIGN.md — GTS Type Hierarchy](../DESIGN.md) — defines the GTS type hierarchy for Function and Workflow entities
* Function definition and registration uses `gts.x.core.sless.function.v1~` as the base callable type
* Workflow definition uses `gts.x.core.sless.workflow.v1~` as a sibling peer base type; `workflow_traits` are defined on this type exclusively
