---
status: proposed
date: 2026-03-09
---
<!--
 =============================================================================
 ARCHITECTURE DECISION RECORD (ADR) — based on MADR format
 =============================================================================
 PURPOSE: Capture WHY Function was chosen as the base callable type rather
 than an abstract Entrypoint type, and why Workflow extends Function.

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
# ADR — Function as Base Callable Type (Function → Workflow Hierarchy)

**ID**: `cpt-cf-serverless-runtime-adr-callable-type-hierarchy`

## Context and Problem Statement

The Serverless Runtime domain model needs a GTS type hierarchy for callable entities — functions and workflows. The original proposal used an abstract `Entrypoint` base type with `Function` and `Workflow` as siblings (`entrypoint → function | workflow`). This ADR addresses whether to retain that hierarchy or adopt a `function → workflow` model where Function is the base callable and Workflow extends it.

A function, in the general sense, is something that accepts inputs, produces outputs, and may have side effects. Both plain functions and workflows share these fundamental characteristics — they are defined with input/output schemas, invoked with parameters, and return results. Workflows add specific capabilities on top (durable state, compensation, event waiting), but they do not change the fundamental nature of what they are: functions with additional traits.

## Decision Drivers

* A workflow is a type of function — both accept inputs, produce outputs, and may have side effects; the type hierarchy should reflect this
* Functions serve multiple roles: direct invocation targets, helper/utility functions called by other functions, abstract base definitions extended by adapters — not all functions are "entrypoints"
* The type hierarchy must support GTS type matching for adapter routing and capability negotiation
* Shared fields (schema, limits, retry, rate limit, implementation) are common to all callables and belong on the base type
* Workflow-specific traits (checkpointing, compensation, event waiting) are additive capabilities on top of function semantics
* The model must remain runtime-agnostic — no single executor implementation should dictate the type system

## Considered Options

* **Option A**: Function → Workflow (function as base type)
* **Option B**: Entrypoint → Function | Workflow (abstract entrypoint base)
* **Option C**: Callable → Function | Workflow (abstract callable base)

## Decision Outcome

Chosen option: **"Option A: Function → Workflow"**, because a workflow is fundamentally a type of function — both accept inputs, produce outputs, and may have side effects; workflows simply add durable execution capabilities on top. This makes Function the natural base type, produces a simpler two-type hierarchy without an abstract base that is never instantiated directly, and avoids naming the base type after a usage pattern ("entrypoint") that doesn't apply to all callables.

### Consequences

* All callable entities are GTS-typed as functions (`gts.x.core.serverless.function.v1~`), regardless of whether they are invoked directly, used as helpers, or serve as abstract base definitions for extension
* Workflows are a specialization of function that adds durable execution traits — their GTS type (`gts.x.core.serverless.function.v1~x.core.serverless.workflow.v1~`) inherits all function fields
* Adapters that support only functions can match on `function.v1~`; adapters that support workflows match on the derived `workflow.v1~` type — GTS type matching remains sufficient for routing
* No abstract base type exists that is never instantiated, eliminating the "phantom type" that only carries shared fields
* The function/workflow distinction is structural (workflow adds `workflow_traits`) — execution mode (sync vs async) is orthogonal to type

### Confirmation

* DESIGN.md entity summary and GTS schemas use `function.v1~` as the base callable type
* No GTS type in the hierarchy is named after a usage pattern (entrypoint, invocation mode, etc.)
* Adapter capability routing works via GTS type matching on `function.v1~` and `workflow.v1~`
* Code review verifies that SDK types use Function as the base struct with Workflow extending it

## Pros and Cons of the Options

### Option A: Function → Workflow (function as base type)

Function is the base callable type. Workflow extends Function with additional traits for durable execution.

**GTS hierarchy:**
```
gts.x.core.serverless.function.v1~                                          — base
gts.x.core.serverless.function.v1~x.core.serverless.workflow.v1~            — derived
```

| | Aspect | Note |
|---|--------|------|
| Pro | "Function" accurately describes all callables | They accept inputs, produce outputs, and may have side effects |
| Pro | Two types instead of three | Simpler cognitive model and fewer abstractions |
| Pro | All callable roles share the base type naturally | Helpers, abstract bases, and direct targets are all functions — none are "entrypoints" |
| Pro | A workflow **is** a function | It simply adds durable execution capabilities on top; GTS derivation models this "is-a" directly |
| Pro | Shared fields live on Function | The type that is actually instantiated, not a phantom abstract base |
| Neutral | Adapters wanting only plain functions need negative matching | Must exclude `workflow.v1~` derived types rather than positive-match a dedicated type |
| Con | "All workflows are functions" may initially surprise | Though the shared input→output nature makes the relationship accurate |

### Option B: Entrypoint → Function | Workflow (abstract entrypoint base)

An abstract Entrypoint type carries shared fields. Function and Workflow are sibling derived types.

**GTS hierarchy:**
```
gts.x.core.serverless.entrypoint.v1~                                         — abstract base
gts.x.core.serverless.entrypoint.v1~x.core.serverless.function.v1~           — derived
gts.x.core.serverless.entrypoint.v1~x.core.serverless.workflow.v1~           — derived
```

| | Aspect | Note |
|---|--------|------|
| Pro | Function and Workflow evolve independently | Sibling types at the type level |
| Pro | Positive GTS type matching for adapters | Function-only and workflow-only adapters are first-class citizens |
| Con | "Entrypoint" names the base after a usage pattern | Helpers, abstract bases, and utilities are not "entrypoints" but must derive from this type |
| Con | Not every callable is an entry point | The name misrepresents what the entity is |
| Con | Abstract base is never instantiated directly | Phantom type exists only to carry shared fields |
| Con | Three types instead of two | Increased complexity without proportional benefit |

### Option C: Callable → Function | Workflow (abstract callable base)

Same structure as Option B but with a neutral name that avoids the "entrypoint" naming issue.

**GTS hierarchy:**
```
gts.x.core.serverless.callable.v1~                                           — abstract base
gts.x.core.serverless.callable.v1~x.core.serverless.function.v1~             — derived
gts.x.core.serverless.callable.v1~x.core.serverless.workflow.v1~             — derived
```

| | Aspect | Note |
|---|--------|------|
| Pro | "Callable" is a neutral name | Doesn't imply a specific usage pattern |
| Pro | Function and Workflow are explicit sibling types | Independent evolution |
| Con | "Callable" is a synonym for "function" | The distinction adds a type without adding meaning |
| Con | Three types instead of two | Abstract base is never instantiated directly |
| Con | Shared fields live on a type never used directly | Creates indirection for implementers |

## More Information

The previous project's `Entrypoint` type was introduced when all callables were assumed to be top-level invocation targets. As the domain evolved to include helper functions, abstract base definitions for adapter extension, and utility functions that are called by other functions (not directly invoked by users), the "entrypoint" name became misleading.

The "function" concept is well-established across computing: something that takes inputs and produces outputs, potentially with side effects. Both plain functions and workflows fit this definition — a workflow is a function whose side effects include durable state management, compensation, and event-driven continuation. This is why the type hierarchy models Workflow as a specialization of Function rather than a sibling: the relationship is genuinely "is-a", not "shares-fields-with".

The execution mode (sync vs async) and invocation role (direct target vs helper vs abstract base) are orthogonal to type identity and are handled via invocation parameters and runtime configuration, not via the GTS type hierarchy.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

* `cpt-cf-serverless-runtime-design-domain-model` — Defines the GTS type hierarchy for Function and Workflow entities
* `cpt-cf-serverless-runtime-fr-003` — Function definition and registration uses Function as the base callable type
* `cpt-cf-serverless-runtime-fr-004` — Workflow definition extends Function with workflow_traits
