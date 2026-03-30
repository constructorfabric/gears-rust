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

# ADR — `ServerlessSdkError` as a Closed Enum (Not a Trait-Object or Opaque Error)


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: `#[non_exhaustive]` enum with `thiserror`](#option-a-non_exhaustive-enum-with-thiserror)
  - [Option B: `Box<dyn Error + Send + Sync>`](#option-b-boxdyn-error--send--sync)
  - [Option C: `anyhow::Error`](#option-c-anyhowerror)
  - [Option D: Rich trait-based error hierarchy](#option-d-rich-trait-based-error-hierarchy)
- [More Information](#more-information)
- [Non-Applicable Domains](#non-applicable-domains)
- [Review Conditions](#review-conditions)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-serverless-sdk-core-adr-error-enum`

## Context and Problem Statement

`Handler::call` and `WorkflowHandler::compensate` return
`Result<O, ServerlessSdkError>`. The error type must satisfy two requirements that
are in tension:

1. **Handler ergonomics**: function authors must be able to return meaningful error
   information (business error, invalid input, timeout, internal failure) without
   depending on adapter or runtime types.
2. **Adapter contract**: the adapter must translate the returned error into a
   `RuntimeErrorPayload` with a specific `RuntimeErrorCategory` — the category
   determines retry behaviour. The mapping must be deterministic; ambiguous error
   types that produce inconsistent categories break retry logic.

Four structural approaches exist:
1. **Closed enum** — a fixed set of named variants (`UserError`, `InvalidInput`,
   `Timeout`, `NotSupported`, `Internal`), each with a documented `RuntimeErrorCategory`.
2. **Trait object** — `Box<dyn Error + Send + Sync>` as the error type; adapters
   inspect the boxed value or its `Display` to determine category.
3. **`anyhow::Error`** — an opaque, context-chain-carrying error type widely used in
   Rust applications.
4. **Trait-based hierarchy** — a `ServerlessError` trait with an associated
   `category()` method; function authors implement the trait for their error types.

Which approach should the SDK use for `ServerlessSdkError`?

## Decision Drivers

* **[P1]** The adapter must produce a deterministic `RuntimeErrorCategory` from every
  error returned by a handler — ambiguity in error-to-category mapping causes incorrect
  retry behaviour, which is a correctness requirement, not an ergonomic preference.
* **[P1]** The crate must have zero engine-specific dependencies
  (`cpt-cf-serverless-sdk-core-constraint-no-engine-deps`) — the error type must be
  self-contained and not require adapter or runtime types in its definition.
* **[P2]** Function authors should be able to return a meaningful error from their
  handler without implementing a custom trait or wrapping in an adapter type —
  ergonomics drive adoption and reduce the surface for misuse.
* **[P2]** The error type must be compatible with the `?` operator and `std::error::Error`
  so that handler implementations can propagate errors from internal library calls.
* **[P3]** The crate follows the minimal-surface principle
  (`cpt-cf-serverless-sdk-core-principle-minimal-surface`); the error model should
  not be richer than what the adapter contract requires.

## Considered Options

* **Option A**: `#[non_exhaustive]` enum `ServerlessSdkError` with `thiserror` — five
  variants (`UserError`, `InvalidInput`, `Timeout`, `NotSupported`, `Internal`), each
  mapping to a documented `RuntimeErrorCategory`.
* **Option B**: `Box<dyn Error + Send + Sync>` — the `Result` error type is a trait
  object; adapters inspect the boxed error at runtime to determine category.
* **Option C**: `anyhow::Error` — an opaque, context-carrying error type; adapters
  cannot distinguish error categories without convention (e.g., downcasting to
  well-known types).
* **Option D**: Rich trait-based error hierarchy — a `ServerlessError` trait with a
  `category() -> RuntimeErrorCategory` method; function authors implement the trait
  for their domain error types.

## Decision Outcome

Chosen option: **Option A: `#[non_exhaustive]` enum `ServerlessSdkError`**, because
it makes the adapter-to-runtime error category mapping explicit, exhaustive, and
verifiable at compile time. Each variant carries a documented `RuntimeErrorCategory`,
so adapters can match on the variant and produce the correct retry decision without
runtime inspection or convention-based downcasting. The `#[non_exhaustive]` attribute
preserves the ability to add new variants in future semver-compatible releases. The
five variants cover the full space of handler failure semantics required by the PRD
without over-engineering a hierarchy for a bounded error space.

### Consequences

* Function authors map their domain errors to one of the five `ServerlessSdkError`
  variants before returning from `call` or `compensate`. This is intentional: the
  handler is the boundary between domain logic and the platform's error contract.
  Authors who wish to preserve their domain error should wrap it in the `message`
  field of the appropriate variant (e.g., `UserError(format!("{e}"))`) or use `.map_err()`.
* The `?` operator works with `ServerlessSdkError` via `From` implementations that
  authors add in their own crate (e.g., `impl From<MyDomainError> for ServerlessSdkError`).
  The SDK itself does not provide blanket `From` impls, because the correct variant
  mapping is domain-specific knowledge that only the function author holds.
* Adapters `match` on `ServerlessSdkError` variants with an exhaustive (plus `#[non_exhaustive]`
  catch-all) pattern to determine `RuntimeErrorCategory`. The mapping table in DESIGN.md
  §3.1 is the authoritative reference; adapters must implement it exactly.
* When a new variant is added to `ServerlessSdkError`, all adapters must add a `match`
  arm for the new variant. Because `#[non_exhaustive]` requires a catch-all `_` arm,
  existing adapter code will not break at compile time — but the catch-all arm will
  silently mis-categorise the new variant until the adapter is updated. There is no
  compile-time signal for this gap; adapter maintainers must consult the DESIGN.md §3.1
  mapping table when updating the SDK dependency.
* `anyhow` is intentionally excluded as a dependency. Function authors who use `anyhow`
  internally must convert at the handler boundary.

### Confirmation

* `ServerlessSdkError` is declared with `#[non_exhaustive]` — verified by inspection
  of `error.rs`.
* Every variant has a documented `RuntimeErrorCategory` in its doc comment — verified
  by `cargo doc --no-deps` producing no missing-doc warnings.
* No `From<anyhow::Error>` or `From<Box<dyn Error>>` implementation exists in the
  crate — verified by searching `error.rs` and `lib.rs`.
* The DESIGN.md §3.1 error-to-category mapping table covers all five variants — verified
  by cross-referencing the table against the enum definition.

## Pros and Cons of the Options

### Option A: `#[non_exhaustive]` enum with `thiserror`

Five named variants; each maps to a documented `RuntimeErrorCategory`.

* Good, because the adapter-to-runtime category mapping is explicit and exhaustive —
  no runtime inspection, no convention-based downcasting.
* Good, because `thiserror` generates `std::error::Error + Display` impls automatically,
  making the error type compatible with the broader Rust error-handling ecosystem.
* Good, because `#[non_exhaustive]` allows new variants to be added without a semver break.
* Good, because `thiserror` is already a workspace dependency — zero new crate surface.
* Good, because each variant's semantics are documented in the SDK, making the error
  contract discoverable without reading adapter or runtime source.
* Neutral, because function authors must explicitly map domain errors to SDK variants.
  Accepted: the boundary is the right place to do this mapping.
* Bad, because adding a new category (e.g., `RateLimit`) requires a new variant and
  a corresponding adapter update. Mitigated by `#[non_exhaustive]` catch-all requirement.

### Option B: `Box<dyn Error + Send + Sync>`

The `Result` error type is a trait object; category determined by adapter inspection.

* Good, because function authors return any `std::error::Error` implementation directly,
  with no wrapping or mapping required.
* Good, because the SDK introduces no new type; `Box<dyn Error + Send + Sync>` is standard Rust.
* Bad, because the adapter has no deterministic way to determine `RuntimeErrorCategory`
  from an arbitrary `dyn Error`. Any convention (e.g., checking `Display` strings,
  downcasting to known types) is fragile and non-exhaustive.
* Bad, because retry behaviour becomes dependent on the adapter's inspection logic rather
  than the function author's explicit intent — a correctness risk for the platform.
* Bad, because `Box<dyn Error>` does not convey any retry or categorisation semantics;
  the adapter contract between SDK and runtime is implicit rather than typed.

### Option C: `anyhow::Error`

An opaque, context-chain-carrying error type.

* Good, because `anyhow` is widely used in Rust applications; function authors who
  already use `anyhow` can return errors with no conversion.
* Good, because context chains (`with_context`) produce rich error messages for logs.
* Bad, because `anyhow::Error` is opaque — adapters cannot determine `RuntimeErrorCategory`
  without downcasting to concrete types, which requires knowing all possible error types
  in advance (defeating the purpose of opaque errors).
* Bad, because adding `anyhow` as a dependency of the SDK core crate makes it a
  mandatory transitive dependency for every function author and adapter — a non-trivial
  addition for a `no_std`-adjacent or minimal-dependency deployment.
* Bad, because the SDK's minimal-surface principle (`cpt-cf-serverless-sdk-core-principle-minimal-surface`)
  is violated: `anyhow` brings a full error context chain infrastructure to a crate that
  only needs to express five distinct failure categories.

### Option D: Rich trait-based error hierarchy

A `ServerlessError` trait with a `category() -> RuntimeErrorCategory` method;
function authors implement the trait.

* Good, because function authors retain their domain error types and add a trait impl —
  no conversion or wrapping at the handler boundary.
* Good, because adapters call `err.category()` for a deterministic, author-supplied
  category — no inspection needed.
* Bad, because `RuntimeErrorCategory` must be defined in the SDK crate for the trait
  to reference it, exposing an adapter-internal concept (`Retryable`, `NonRetryable`,
  `ResourceLimit`, `Canceled`) to function authors. This couples handler authoring to
  adapter runtime concepts, violating `cpt-cf-serverless-sdk-core-principle-impl-agnostic`.
* Bad, because function authors must implement a SDK trait on their domain error types —
  more boilerplate than returning a named variant.
* Bad, because the minimal-surface principle is violated: a new public trait is added
  to satisfy a use case (preserving domain error types at the handler boundary) that
  can be satisfied with `From` impls defined in the author's own crate.

## More Information

The five `ServerlessSdkError` variants (`UserError`, `InvalidInput`, `Timeout`,
`NotSupported`, `Internal`) map one-to-one to the `RuntimeErrorCategory` values used
by the Serverless Runtime's retry policy engine. Two additional categories
(`ResourceLimit`, `Canceled`) are never produced by handler code — they are
exclusively adapter/runtime signals. This closed set is documented in DESIGN.md §3.1
(ServerlessSdkError → RuntimeErrorCategory Mapping).

Unlike `aws_lambda_runtime::Error` (an opaque `Box<dyn Error>` trait object, equivalent
to Option B above), `ServerlessSdkError` is a structured enum: CyberFabric's retry
semantics require the adapter to know the failure category at categorisation time, not
merely at log time.

## Non-Applicable Domains

| Domain | Disposition | Reasoning |
|--------|-------------|-----------|
| PERF | N/A | Decision concerns error type shape; enum variant matching is negligible cost relative to handler I/O latency and is not a decision driver |
| SEC | N/A | Decision concerns error type ergonomics; error variants carry string messages, not credentials or PII — no authentication/authorization concern |
| REL | Addressed in Decision Outcome | Error-to-retry-category mapping is a reliability concern; addressed by the deterministic variant mapping |
| DATA | N/A | Error type is an in-memory enum; no persistence, no schema ownership |
| OPS | N/A | Pure library; no deployment topology, monitoring, or infrastructure concern |
| COMPL | N/A | Internal developer tooling; no regulatory requirement |
| UX | N/A | No end-user UI; developer ergonomics addressed in Decision Outcome and Pros/Cons |
| BIZ | N/A | Internal Rust library; no business stakeholder buy-in or cost analysis applicable |

## Review Conditions

| Trigger | Action |
|---------|--------|
| The Serverless Runtime introduces a new `RuntimeErrorCategory` value (e.g., `RateLimit` as a distinct category from `ResourceLimit`) | Add a corresponding `ServerlessSdkError` variant; this requires a semver-compatible minor release and adapter updates |
| A significant proportion of function authors report friction converting domain errors to SDK variants | Re-evaluate Option D (trait-based hierarchy) or introduce a `ServerlessSdkError::from_error(category, source)` convenience constructor |
| `anyhow` or `eyre` becomes a workspace-wide standard error crate | Re-evaluate Option C; assess whether category-carrying context extensions can satisfy the adapter contract deterministically |

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

* `cpt-cf-serverless-sdk-core-fr-error-model` — `ServerlessSdkError` enum definition and variant set
* `cpt-cf-serverless-sdk-core-constraint-no-engine-deps` — error type defined without adapter/runtime imports
* `cpt-cf-serverless-sdk-core-principle-minimal-surface` — five variants, no trait hierarchy, no opaque wrapper
* `cpt-cf-serverless-sdk-core-component-error` — responsibility scope of `error.rs`
* DESIGN.md §3.1 `ServerlessSdkError → RuntimeErrorCategory Mapping` — the authoritative variant-to-category table
