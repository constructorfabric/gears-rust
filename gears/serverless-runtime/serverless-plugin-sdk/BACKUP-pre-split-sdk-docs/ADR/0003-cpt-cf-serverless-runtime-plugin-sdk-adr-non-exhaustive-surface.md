---
status: accepted
date: 2026-05-18
---

# ADR-0003: `#[non_exhaustive]` and Mandatory Constructors on Stable Surface Types

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Operating Envelope](#operating-envelope)
  - [Confirmation](#confirmation)
  - [Applicability](#applicability)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Non-exhaustive everywhere with mandatory constructors](#non-exhaustive-everywhere-with-mandatory-constructors)
  - [Exhaustive types + struct-literal construction](#exhaustive-types--struct-literal-construction)
  - [Seal traits only; leave structs exhaustive](#seal-traits-only-leave-structs-exhaustive)
- [More Information](#more-information)
  - [Related Decisions](#related-decisions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-serverless-runtime-plugin-sdk-adr-non-exhaustive-surface`

## Context and Problem Statement

Every SDK domain type crosses at least three crate boundaries: the host crate, every runtime plugin crate, and the conformance suite. Additive schema evolution (a new field on `InvocationIndexEvent`, a new variant on `RuntimeErrorCategory`) must not force every adapter to recompile against breaking changes. The SDK needs a uniform stability discipline that makes additive evolution safe by default.

## Decision Drivers

- Adapter authors and the host depend on the same SDK crate; schema drift breaks them in lockstep unless the API is forward-compatible.
- Pre-1.0 (per the [PRD Compatibility policy](../PRD.md#operational-concept--environment)) the SDK is allowed to evolve, but adapters in the tree should keep building across additive changes within a minor version.
- The host's invocation index schema may grow over time; adapters must not be forced to re-emit when a new optional field appears.
- The conformance suite ships with the SDK and must compile against multiple minor versions during transition windows.

## Considered Options

- `#[non_exhaustive]` on every public struct and enum + mandatory `::new(...)` (or builder) constructors.
- Exhaustive types + permitted struct-literal construction outside the crate.
- Seal traits only; leave structs and enums exhaustive.

## Decision Outcome

Chosen: **`#[non_exhaustive]` on every public struct and enum in the SDK's domain modules, with a required public construction path**, because it is the only option that makes additive field and variant additions backward-compatible without an SDK major bump.

### Consequences

- Every domain struct (`InvocationRecord`, `CompensationContext`, `Context`, `CompensationInput`, `Schedule`, `Trigger`, `InvocationIndexEvent`, `RuntimeErrorPayload`) carries `#[non_exhaustive]` and exposes at least one **documented public construction path**. The default is `::new(...)` when the required field set is small and fixed, or a builder when there are optional knobs or invariant-validation steps that benefit from a fluent API. `Default`, `From`, and `TryFrom` impls also count as valid construction paths and are preferred over a redundant `::new` when they fit. Struct-literal construction outside the crate is rejected by the compiler.
- Constructors carry `#[must_use]` so a caller cannot accidentally drop the constructed value.
- `Default` is allowed and encouraged for types with sensible defaults. `#[non_exhaustive]` only bars *external* struct literals; internal `Default::default()` is unaffected and gives external callers a zero-argument path when one makes sense.
- Every domain enum (`ServerlessSdkError`, `RuntimeErrorCategory`, `TimelineEventType`, `CompensationTrigger`) is `#[non_exhaustive]`. Pattern matches in *external* crates must include a `_` arm; matches *inside* the SDK crate (including the conformance suite that lives there) remain exhaustive. Adapter-side test code is external and pays the `_`-arm cost.
- Known trade on the error and category enums (`ServerlessSdkError`, `RuntimeErrorCategory`): a `_` arm in adapter code silently absorbs any future variant. Adding a new variant in a minor version keeps adapter source compiling, but adapter logic that had no behavior for that variant now routes it through `_` — usually a generic fallback. The discipline trades loud failure on new variants for silent source compatibility. Acceptable here on the expectation that adapter `_` arms log and forward unknown variants rather than swallow them; if a future audit shows adapters absorbing unknown variants silently, the error enums become a candidate for exhaustive treatment in a follow-up ADR.
- Removing a field or renaming a variant remains a major-version change. The soft-removal path before a hard rename is to mark the field or variant `#[deprecated]` and continue to honor reads; this stays additive and does not require a major bump on its own.
- The compatibility expectation this discipline supports is stated in the [PRD Compatibility policy](../PRD.md#operational-concept--environment) — pre-1.0, adapters in-tree should keep building across additive changes within a minor version.

### Operating Envelope

This discipline is sound under the following assumptions; outside them, a narrower rule may be cheaper.

- **In-tree adapter consumers depend on minor-version source compatibility.** The SDK ships with adapters in the same workspace; a minor bump that source-breaks them is expensive. If the SDK ever ships only to external consumers under independent semver, exhaustive types with minor-bumps-on-variant-addition becomes a cheaper alternative and this ADR is a revisit candidate.
- **Adapter `_` arms log unknown variants.** The error-enum trade is acceptable only if adapter authors handle the wildcard arm responsibly. If adapters absorb unknown variants silently, the error enums should be revisited (see the corresponding Consequences bullet).
- **Public domain types only.** The rule applies to public types in the SDK's domain modules. Crate-internal and `pub(crate)` items are not subject to this discipline; over-applying `#[non_exhaustive]` to internal types adds noise without compatibility benefit.

### Confirmation

- Struct-literal construction of an SDK domain type from outside the crate is rejected by the compiler as a direct consequence of `#[non_exhaustive]`; this surfaces at first build of any adapter that drifts from the discipline.
- Integration tests build an example adapter against the previous minor version and the current minor version with the same `Cargo.lock`; both must pass.

### Applicability

| Domain | Status | Notes |
|---|---|---|
| ARCH | Addressed | Cross-crate stability discipline; uniform per type. |
| INT | Addressed | Every public type follows the same construction and pattern-matching contract. |
| MAINT | Addressed | Additive evolution non-breaking; rename or removal still major-bumps. |
| TEST | Addressed (with caveat) | Conformance fixtures and adapter-side tests pay the `_`-arm cost on SDK enums when matched from outside the SDK crate. |
| REL | N/A | No runtime state owned by these types. |
| PERF | N/A | Zero runtime cost — `#[non_exhaustive]` is compile-time only. |
| SEC | N/A | No trust boundary introduced. |
| DATA | N/A | No persistence. |
| OPS | N/A | No deployable surface introduced. |
| COMPL / UX / BIZ | N/A | Internal SDK; no end-user or regulated surface. |

## Pros and Cons of the Options

### Non-exhaustive everywhere with mandatory constructors

- Good, because additive fields and additive variants become non-breaking by construction.
- Good, because a required construction path gives the SDK a place to enforce invariants and defaults.
- Good, because the discipline is uniform across the domain module — one rule, applied everywhere it matters.
- Bad, because every domain type needs a documented public construction path (`::new`, builder, `Default`, `From`, or `TryFrom`) — a one-time authoring cost per type.
- Bad, because matches against SDK enums in external crates require a `_` arm, which loses compile-checked exhaustiveness in adapter code.

### Exhaustive types + struct-literal construction

- Good, because the API is maximally transparent — no hidden defaults.
- Bad, because every additive field is a breaking change requiring a major bump.
- Bad, because struct literals leak field-order assumptions into every adapter.

### Seal traits only; leave structs exhaustive

- Good, because trait surface is locked down.
- Bad, because the dominant evolution risk is on the data types (index event, error payload), not the traits.
- Bad, because partial discipline is worse than no discipline — adapters cannot reason about which types are safe to literal-construct.

## More Information

This ADR is the single source of truth for the SDK's API stability discipline. Specific type-level invariants (which fields are required, which constructors exist) belong in DESIGN §3.3 alongside each trait contract.

### Related Decisions

- **ADR-0004** (`WorkflowHandler: FunctionHandler` supertrait) — `CompensationInput` and other workflow types are `#[non_exhaustive]` structs governed by this discipline.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

- `cpt-cf-serverless-runtime-plugin-sdk-fr-error-model` — `#[non_exhaustive]` `ServerlessSdkError`.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-compensation-input` — `#[non_exhaustive]` struct with `::new(...)` constructor.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-schedule-trigger-types` — `#[non_exhaustive]` `Schedule` and `Trigger`.
- `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-client-index-events` — `#[non_exhaustive]` `InvocationIndexEvent`.
- `cpt-cf-serverless-runtime-plugin-sdk-interface-handler-trait` — invariants reference `#[non_exhaustive]`.
- `cpt-cf-serverless-runtime-plugin-sdk-interface-workflow-trait` — invariants reference `#[non_exhaustive]`.
- `cpt-cf-serverless-runtime-plugin-sdk-interface-runtime-adapter` — invariants reference `#[non_exhaustive]`.
- `cpt-cf-serverless-runtime-plugin-sdk-interface-runtime-client` — invariants reference `#[non_exhaustive]`.
- `cpt-cf-serverless-runtime-plugin-sdk-principle-minimal-surface` — every public type carries the same stability discipline.
