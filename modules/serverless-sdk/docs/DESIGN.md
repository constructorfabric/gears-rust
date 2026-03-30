# Technical Design — CyberFabric Serverless SDK Core


<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions & Sequences](#36-interactions--sequences)
  - [3.7 Testability Architecture](#37-testability-architecture)
  - [Database schemas & tables](#database-schemas--tables)
  - [3.8 Capacity, Cost, and Deployment Exclusions](#38-capacity-cost-and-deployment-exclusions)
- [4. Additional Context](#4-additional-context)
  - [Relationship to the Serverless Runtime Design](#relationship-to-the-serverless-runtime-design)
  - [Known Technical Debt](#known-technical-debt)
  - [Crate Naming Convention](#crate-naming-convention)
- [5. Non-Applicable Domains](#5-non-applicable-domains)
- [6. Traceability](#6-traceability)

<!-- /toc -->

<!--
=============================================================================
TECHNICAL DESIGN DOCUMENT
=============================================================================
PURPOSE: Define HOW the system is built — architecture, components, APIs,
data models, and technical decisions that realize the requirements.

DESIGN IS PRIMARY: DESIGN defines the "what" (architecture and behavior).
ADRs record the "why" (rationale and trade-offs) for selected design
decisions; ADRs are not a parallel spec, they are traceability artifacts.

SCOPE:
  ✓ Architecture overview and vision
  ✓ Design principles and constraints
  ✓ Component model and interactions
  ✓ API contracts and interfaces
  ✓ Data models

NOT IN THIS DOCUMENT (see other templates):
  ✗ Requirements → PRD.md
  ✗ Detailed rationale for decisions → ADR/
  ✗ Step-by-step implementation flows → features/

STANDARDS ALIGNMENT:
  - IEEE 1016-2009 (Software Design Description)
  - IEEE 42010 (Architecture Description)
  - ISO/IEC 15288 / 12207 (Architecture & Design Definition processes)

DESIGN LANGUAGE:
  - Be specific and clear; no fluff, bloat, or emoji
  - Reference PRD requirements using `cpt-cf-serverless-sdk-core-fr-{slug}` IDs
  - Reference ADR documents using `cpt-cf-serverless-sdk-core-adr-{slug}` IDs
=============================================================================
-->

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-design-root`
## 1. Architecture Overview

### 1.1 Architectural Vision

`cf-serverless-sdk-core` is a pure Rust library that defines the stable, engine-agnostic
authoring contract for CyberFabric serverless functions and workflows. It has no runtime
state, no I/O, and no external service dependencies. Its entire surface is a set of Rust
traits and value types that adapter crates drive and function authors implement.

The design organises the public surface into five concern-separated modules (`context`,
`environment`, `error`, `handler`, `workflow`) with a sixth adapter-only module (`trace`)
that isolates all observability wiring. The module boundaries enforce a strict separation:
function authors interact only with the five core modules; adapter developers additionally
use `trace`. No module references an engine-specific type.

The `async-trait` crate is used for the handler and workflow traits, making them ergonomic
to implement and ensuring the `Future` returned from `call` and `compensate` is `+ Send`
without requiring callers to annotate anything
(`cpt-cf-serverless-sdk-core-adr-async-trait`).

### 1.2 Architecture Drivers

#### Functional Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-serverless-sdk-core-fr-handler-trait` | `Handler<I, O>` generic async trait in `handler.rs` |
| `cpt-cf-serverless-sdk-core-fr-handler-send-sync` | `Handler: Send + Sync + 'static` bound on the trait definition |
| `cpt-cf-serverless-sdk-core-fr-workflow-handler-trait` | `WorkflowHandler<I, O>: Handler<I, O>` supertrait in `workflow.rs` |
| `cpt-cf-serverless-sdk-core-fr-compensation-input` | `CompensationInput` struct in `workflow.rs`, `#[non_exhaustive]` |
| `cpt-cf-serverless-sdk-core-fr-context` | `Context` struct in `context.rs` with 9 fields from `InvocationRecord` |
| `cpt-cf-serverless-sdk-core-fr-deadline-helpers` | `is_deadline_exceeded()` and `remaining_time()` on `Context` |
| `cpt-cf-serverless-sdk-core-fr-environment-trait` | Sync `Environment` trait in `environment.rs` (`cpt-cf-serverless-sdk-core-adr-sync-environment`) |
| `cpt-cf-serverless-sdk-core-fr-error-model` | `#[non_exhaustive]` `ServerlessSdkError` with `thiserror` in `error.rs` |
| `cpt-cf-serverless-sdk-core-fr-trace-module` | `trace.rs` with `call_instrumented` and `compensate_instrumented` |
| `cpt-cf-serverless-sdk-core-fr-no-consumer-tracing` | `tracing` calls contained entirely within `trace.rs` |

#### NFR Allocation

| NFR ID | NFR Summary | Allocated To | Design Response | Verification Approach |
|--------|-------------|--------------|-----------------|----------------------|
| `cpt-cf-serverless-sdk-core-nfr-no-engine-deps` | No engine-specific deps | All modules | Dep list restricted to `serde`, `serde_json`, `thiserror`, `async-trait`, `tracing` | `cargo deny` in CI |
| `cpt-cf-serverless-sdk-core-nfr-stable-rust` | Stable Rust 1.92.0 | `handler.rs`, `workflow.rs` | `async-trait` used instead of unstable RPITIT with `Send` bound | `cargo check` on stable in CI |
| `cpt-cf-serverless-sdk-core-nfr-no-unsafe` | Zero `unsafe` blocks | All modules | Workspace `unsafe_code = "forbid"` lint; no pointer manipulation | Lint enforced at compile time |
| `cpt-cf-serverless-sdk-core-nfr-low-overhead` | No blocking I/O or extra heap allocs on hot path | `trace.rs`, `handler.rs` | `call_instrumented` introduces one `Box<dyn Future>` (async-trait) and one `tracing` span; no additional heap allocations on the hot path | Criterion benchmark in CI |
| `cpt-cf-serverless-sdk-core-nfr-api-docs` | Zero missing-doc warnings; `#![deny(missing_docs)]` | All public items | All public types, traits, and functions documented with purpose, usage, and invariants | `cargo doc --no-deps` in CI |
| `cpt-cf-serverless-sdk-core-nfr-authoring-ergonomics` | Plain `async fn` syntax; no lifetime annotations on handler impls | `handler.rs`, `workflow.rs` | `async-trait` expands `async fn` to `Pin<Box<dyn Future + Send>>` internally, keeping the `impl` surface annotation-free (`cpt-cf-serverless-sdk-core-adr-async-trait`) | SDK examples and integration tests compile with `async fn` syntax; CI fails on any explicit `impl Future` or lifetime annotation on method signatures |

#### Key ADRs

| ADR ID | Decision Summary |
|--------|-----------------|
| `cpt-cf-serverless-sdk-core-adr-async-trait` | Use `async-trait` over RPITIT for `Handler` and `WorkflowHandler` |
| `cpt-cf-serverless-sdk-core-adr-sync-environment` | `Environment` interface is synchronous (adapter pre-fetch model) |
| `cpt-cf-serverless-sdk-core-adr-compensation-input` | `CompensationInput` is a structured type, not a generic handler input |
| `cpt-cf-serverless-sdk-core-adr-context-struct` | `Context` is a concrete struct, not a trait or generic parameter |
| `cpt-cf-serverless-sdk-core-adr-error-enum` | `ServerlessSdkError` is a `#[non_exhaustive]` enum, not a trait object or opaque error |
| `cpt-cf-serverless-sdk-core-adr-workflow-supertrait` | `WorkflowHandler<I,O>: Handler<I,O>` supertrait, not an independent trait |

### 1.3 Architecture Layers

```
╔══════════════════════════════════════════════════════╗
║  Function / Workflow Author                          ║
║  implements Handler<I, O> or WorkflowHandler<I, O>   ║
╚═══════════════════════╤══════════════════════════════╝
                        │ uses
╔═══════════════════════▼══════════════════════════════╗
║  cf-serverless-sdk-core  (this crate)                ║
║  ┌──────────┐ ┌───────────┐ ┌───────┐ ┌──────────┐  ║
║  │ handler  │ │ workflow  │ │ error │ │ context  │  ║
║  └──────────┘ └───────────┘ └───────┘ └──────────┘  ║
║  ┌─────────────────┐ ┌───────────────────────────┐   ║
║  │  environment    │ │  trace  (adapter-only)     │   ║
║  └─────────────────┘ └───────────────────────────┘   ║
╚═══════════════════════╤══════════════════════════════╝
                        │ drives via traits
╔═══════════════════════▼══════════════════════════════╗
║  Adapter crate (cf-serverless-sdk-adapter-*)         ║
║  (Temporal, Starlark, cloud FaaS — out of scope)      ║
╚══════════════════════════════════════════════════════╝
```

| Layer | Responsibility | Technology |
|-------|---------------|------------|
| Handler Author | Implements `Handler<I, O>` / `WorkflowHandler<I, O>` | Rust + `async-trait` |
| SDK Core (this crate) | Defines traits, types, error model, instrumentation | Rust stable, `serde`, `tracing` |
| Adapter | Drives handlers, populates `Context` and `Environment`, emits spans | Out of scope (future adapter crates) |

---

## 2. Principles & Constraints

### 2.1 Design Principles

#### Implementation-Agnostic Authoring Contract

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-principle-impl-agnostic`

No engine-specific type appears in the public API of this crate. `Context`, `Environment`,
`CompensationInput`, and `ServerlessSdkError` are entirely defined in terms of stable
platform concepts (GTS IDs as `String`, `serde_json::Value` for opaque payloads) without
depending on any adapter. This directly enforces
`cpt-cf-serverless-runtime-principle-impl-agnostic` at the SDK layer.

**ADRs**: `cpt-cf-serverless-sdk-core-adr-sync-environment` — `Environment` is synchronous
precisely because an async interface would require the trait to hold a live credstore client,
violating this principle.

#### GTS Identity by Reference

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-principle-gts-by-reference`

GTS type IDs (`function_id`, `error_type_id`, GTS chain strings) are carried as opaque
`String` values throughout the crate. The SDK never interprets, parses, or validates GTS
chains. This prevents coupling to GTS library versions and keeps the crate portable.

#### Minimal Trusted Surface

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-principle-minimal-surface`

The crate exposes exactly the types and traits required for handler authoring and adapter
driving. No utility types, convenience wrappers, or domain-specific helpers are added
unless they directly serve a stated requirement. Every public item must be justifiable
by a PRD requirement ID.

### 2.2 Constraints

#### No Engine Dependencies — Ever

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-constraint-no-engine-deps`

The `[dependencies]` section of `Cargo.toml` must never include engine-specific crates.
This constraint is permanent: adding an engine dependency invalidates the adapter
portability guarantee and breaks the implementation-agnostic principle.

**ADRs**: `cpt-cf-serverless-sdk-core-adr-async-trait`

#### SDK Trust Boundary — All Inputs Are Trusted

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-constraint-trust-boundary`

The SDK accepts all inputs it receives as trusted. Specifically:

- `Context` fields (`tenant_id`, `invocation_id`, `correlation_id`, etc.) are populated
  by the adapter from the runtime's `InvocationRecord`; the SDK does not validate them.
- `input: I` is a value the adapter has already deserialised from the runtime's `params`
  JSON; the SDK does not validate the deserialized value's business invariants.
- `env: &dyn Environment` is a pre-populated snapshot provided by the adapter; the SDK
  does not verify secret resolution or access control.

Input validation (schema conformance, injection prevention, privilege constraints) is the
responsibility of the Serverless Runtime API layer and adapter before the handler is called.
Function authors are responsible for validating *business* invariants on `input: I`
within their `call` implementation and returning `ServerlessSdkError::InvalidInput` if
those invariants are violated.

#### Stable Rust — No Nightly Features

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-constraint-stable-rust`

The crate must compile on the workspace minimum Rust version without any nightly features,
attributes, or unstable library APIs. Design decisions that require nightly (e.g., RPITIT
with `Send` bounds before stabilisation) must be replaced with stable alternatives.

**ADRs**: `cpt-cf-serverless-sdk-core-adr-async-trait`

---

## 3. Technical Architecture

### 3.1 Domain Model

**Technology**: Rust structs and traits
**Location**: [`cyberfabric-serverless-sdk-core/src/`](../cyberfabric-serverless-sdk-core/src/)

#### InvocationRecord → Context Field Mapping

`Context` is the SDK's read-only projection of the runtime's `InvocationRecord`.
The adapter populates `Context` fields from the record before calling the handler.

| `Context` field | Source in Runtime | Type |
|-----------------|-------------------|------|
| `invocation_id` | `InvocationRecord.invocation_id` | `String` |
| `function_id` | `InvocationRecord.function_id` | `String` (GTS ID) |
| `function_version` | `InvocationRecord.function_version` | `String` |
| `tenant_id` | `InvocationRecord.tenant_id` | `String` |
| `attempt_number` | Adapter-tracked retry count | `u32` (1-indexed) |
| `correlation_id` | `InvocationObservability.correlation_id` | `String` |
| `trace_id` | `InvocationObservability.trace_id` | `Option<String>` |
| `span_id` | `InvocationObservability.span_id` | `Option<String>` |
| `deadline` | Computed from `FunctionLimits.timeout_seconds` at invocation start | `Option<std::time::Instant>` |

**Fields omitted from Context** (runtime concerns, not handler concerns):
`status`, `mode`, `params` (typed via `I`), `result`, `error`, `timestamps`,
`metrics`.

#### CompensationContext → CompensationInput Field Mapping

`CompensationInput` is the SDK's projection of the runtime's `CompensationContext`
(`gts.x.core.serverless.compensation_context.v1~`). The adapter deserialises the
runtime's JSON envelope and populates this struct.

| `CompensationInput` field | Source in Runtime's `CompensationContext` | Type |
|--------------------------|-------------------------------------------|------|
| `trigger` | `trigger` (`"failure"` / `"cancellation"`) | `CompensationTrigger` enum |
| `original_workflow_invocation_id` | `original_workflow_invocation_id` | `String` |
| `failed_step_id` | `failed_step_id` | `String` |
| `failed_step_error` | `failed_step_error` | `Option<FailedStepError>` |
| `workflow_state_snapshot` | `workflow_state_snapshot` | `serde_json::Value` |
| `timestamp` | `timestamp` | `String` (ISO 8601) |
| `function_id` | `invocation_metadata.function_id` | `String` (GTS ID) |
| `original_input` | `invocation_metadata.original_input` | `serde_json::Value` |
| `tenant_id` | `invocation_metadata.tenant_id` | `String` |
| `correlation_id` | `invocation_metadata.correlation_id` | `Option<String>` |
| `started_at` | `invocation_metadata.started_at` | `Option<String>` (ISO 8601) |

`FailedStepError` is a typed projection of the runtime's `failed_step_error` object:
`error_type: String`, `message: String`, `error_metadata: Option<serde_json::Value>`.

Field names match the runtime's `CompensationContext` schema exactly. `correlation_id`
and `started_at` are `Option` because the runtime schema marks them as optional within
`invocation_metadata`.

#### ServerlessSdkError → RuntimeErrorCategory Mapping

Adapters use this mapping to produce the correct `RuntimeErrorPayload` from a
`ServerlessSdkError` returned by a handler.

| `ServerlessSdkError` | `RuntimeErrorCategory` | GTS Error Type Hint |
|----------------------|------------------------|---------------------|
| `UserError(msg)` | `NonRetryable` | `gts.x.core.serverless.err.v1~x.core.serverless.err.validation.v1~` |
| `InvalidInput(msg)` | `NonRetryable` | `gts.x.core.serverless.err.v1~x.core.serverless.err.validation.v1~` |
| `Timeout` | `Timeout` | `gts.x.core.serverless.err.v1~x.core.serverless.err.runtime_timeout.v1~` |
| `NotSupported(msg)` | `NonRetryable` | adapter-defined |
| `Internal(msg)` | `Retryable` | adapter-defined |

**Variant semantics for function authors** — both `UserError` and `InvalidInput` are `NonRetryable`; the distinction is:
- `InvalidInput` — the request violates a structural or type constraint that the handler checked (e.g., a required field is absent, a value is out of allowed range). Return this *before* any side effects.
- `UserError` — the request is structurally valid but rejected by business logic (e.g., insufficient funds, duplicate resource, forbidden action for the caller's state). Return this after business rules are evaluated.

**Adapter-only categories** (never produced by handler code):

| `RuntimeErrorCategory` | Origin | Notes |
|------------------------|--------|-------|
| `ResourceLimit` | Adapter | Tenant quota or resource limit exceeded; adapter signals before or during invocation |
| `Canceled` | Runtime | External cancellation; runtime applies this status, not the handler |

#### API Stability: `#[non_exhaustive]` Surface Summary

All public types in this crate that may gain fields or variants in future semver-compatible
releases are declared `#[non_exhaustive]`. The table below is the authoritative reference
for which types carry this attribute and what it means for each consumer role.

| Type | `#[non_exhaustive]` | Impact on function authors | Impact on adapter authors |
|------|---------------------|--------------------------|--------------------------|
| `ServerlessSdkError` | Yes (enum) | `match` must include a `_` catch-all arm | `match` must include a `_` catch-all arm; no compile-time signal exists for new variants — adapter maintainers must consult DESIGN.md §3.1 when updating the SDK dependency |
| `CompensationInput` | Yes (struct) | Field access by name is stable; struct literal construction outside the crate is forbidden | Adapter constructs `CompensationInput` via `CompensationInput::new(trigger, original_workflow_invocation_id, failed_step_id, failed_step_error, workflow_state_snapshot, timestamp, function_id, original_input, tenant_id, correlation_id, started_at)` — a `pub fn new(...)` constructor defined in the crate |
| `FailedStepError` | Yes (struct) | Field access by name is stable; struct literal construction outside the crate is forbidden | Constructed via `FailedStepError::new(error_type, message, error_metadata)` |
| `CompensationTrigger` | Yes (enum) | `match` must include a `_` catch-all arm | `match` must include a `_` catch-all arm |
| `Context` | No | All 9 fields are stable; struct literal construction is used in tests | Adapter constructs `Context` via struct literal syntax; any field addition is a compile break at every adapter construction site — intentional, to force `InvocationRecord → Context` mapping updates |

**Note on `Context`**: `Context` is not `#[non_exhaustive]` because adapters must
construct it in struct literal form before calling handlers. If a new field is added to
`Context`, adapter code that constructs it with `Context { field_a, field_b, .. }` will
fail to compile, prompting the required update. This is the intended mechanism for
keeping adapter-side `InvocationRecord → Context` mappings in sync.

### 3.2 Component Model

```mermaid
graph TD
    lib["lib.rs (re-exports)"]
    ctx["context.rs\nContext"]
    env["environment.rs\nEnvironment trait"]
    err["error.rs\nServerlessSdkError"]
    hdl["handler.rs\nHandler<I,O>"]
    wfl["workflow.rs\nWorkflowHandler<I,O>\nCompensationInput\nCompensationTrigger"]
    trc["trace.rs\ncall_instrumented\ncompensate_instrumented"]

    lib --> ctx
    lib --> env
    lib --> err
    lib --> hdl
    lib --> wfl
    lib --> trc

    hdl --> ctx
    hdl --> env
    hdl --> err
    wfl --> ctx
    wfl --> env
    wfl --> err
    wfl --> hdl
    trc --> ctx
    trc --> env
    trc --> err
    trc --> hdl
    trc --> wfl
```

#### context.rs — Context

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-component-context`

##### Why this component exists

Handlers need a stable, read-only view of their own invocation identity and execution
constraints. `Context` provides exactly this without exposing the full `InvocationRecord`
or any runtime internals.

##### Responsibility scope

Owns: `Context` struct (9 fields), `is_deadline_exceeded()`, `remaining_time()` helpers.
Derives: `Debug`, `Clone`. `deadline: Option<std::time::Instant>` is `Copy`, so `Context`
is cheaply cloneable for test construction. `is_deadline_exceeded()` and `remaining_time()`
are marked `#[must_use]` — ignoring the return value is a logic error.

##### Responsibility boundaries

Does not own: any mutable invocation state, status transitions, retry tracking, raw `params`
(those come as typed `I` through the handler). Does not parse GTS chains.

---

#### environment.rs — Environment

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-component-environment`

##### Why this component exists

Handlers need access to deployment configuration and secrets without coupling to the
credstore SDK, async resolution, or any platform infrastructure. `Environment` is the
minimal abstraction that satisfies this need synchronously.

##### Responsibility scope

Owns: `Environment` trait with `get_config` and `get_secret`.

##### Responsibility boundaries

Does not own: secret resolution, credstore client, async fetching logic, secret caching.
Those are adapter concerns. `Environment` is a read-only snapshot, not a live proxy.

**ADRs**: `cpt-cf-serverless-sdk-core-adr-sync-environment`

---

#### error.rs — ServerlessSdkError

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-component-error`

##### Why this component exists

Handlers need to express failure semantics (business logic errors, invalid input, timeout,
unsupported operations, internal failures) in a way that unambiguously maps to
`RuntimeErrorCategory` without depending on runtime types.

##### Responsibility scope

Owns: `ServerlessSdkError` enum with 5 `#[non_exhaustive]` variants, each with documented
`RuntimeErrorCategory` mapping. Derives: `Debug`. Implements `Display + std::error::Error`
via `thiserror`. Does **not** derive `Clone` or `PartialEq` — error values are consumed at
the adapter boundary and not compared or cloned in SDK code.

##### Responsibility boundaries

Does not own: `RuntimeErrorPayload` construction (adapter concern), error type GTS ID
assignment (adapter concern), retry logic (runtime concern).

---

#### handler.rs — Handler

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-component-handler`

##### Why this component exists

`Handler<I, O>` is the base callable contract. Every serverless function — stateless or
durable — is a `Handler`. This is the SDK expression of
`cpt-cf-serverless-runtime-principle-unified-function` and the GTS `function.v1~` base type.

##### Responsibility scope

Owns: `Handler<I, O>` async trait with `call` method. Declares `I: DeserializeOwned + Send + 'static`
and `O: Serialize + Send + 'static` bounds. Requires `Self: Send + Sync + 'static`.

The canonical adapter storage pattern is `Arc<dyn Handler<I, O> + Send + Sync>`: shared
ownership across concurrent invocations on a multi-threaded async runtime. `Box<dyn Handler<I, O>>`
is valid for single-owner dispatch but insufficient for shared registry storage.

##### Responsibility boundaries

Does not own: input deserialisation from raw JSON (adapter concern), output serialisation
to `InvocationRecord.result` (adapter concern), span emission (trace module concern).

**ADRs**: `cpt-cf-serverless-sdk-core-adr-async-trait`

---

#### workflow.rs — WorkflowHandler + CompensationInput

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-component-workflow`

##### Why this component exists

Durable workflows require compensation capability (saga pattern, BR-133). `WorkflowHandler`
extends `Handler` with `compensate`, and `CompensationInput` provides the structured context
that compensation handlers receive — expressing the function-level compensation layer without
coupling to executor-specific step-level APIs.

##### Responsibility scope

Owns: `WorkflowHandler<I, O>` trait (extends `Handler<I, O>`) with `compensate` method.
Owns `CompensationInput` struct (11 fields, all `#[non_exhaustive]`), `FailedStepError` struct,
and `CompensationTrigger`
enum (`Failure`, `Cancellation`, `#[non_exhaustive]`).

##### Responsibility boundaries

Does not own: step-level compensation (executor concern), state machine transitions
(`compensating` → `compensated` / `dead_lettered` — runtime concern), serialisation
of `CompensationInput` from the runtime's `CompensationContext` JSON (adapter concern).

**Related**: `cpt-cf-serverless-sdk-core-component-handler`

**ADRs**: `cpt-cf-serverless-sdk-core-adr-compensation-input`

---

#### trace.rs — Instrumentation Utilities

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-component-trace`

##### Why this component exists

The Serverless Runtime emits `InvocationTimelineEvent` records for every invocation.
These must be driven from a consistent, centrally-defined location without SDK consumers
adding any observability code. `trace.rs` is the sole location where `tracing` events
are emitted, and it is intended for adapter use only.

##### Responsibility scope

Owns: `call_instrumented<H, I, O>` and `compensate_instrumented<H, I, O>` free functions.
Each creates a named `tracing::info_span`, records optional `trace_id`/`span_id` fields
lazily, and emits `started`/`succeeded`/`failed` or `compensation_*` lifecycle events
that map to `TimelineEventType` variants.

##### Responsibility boundaries

Does not own: `tracing` subscriber setup (application/adapter concern), span export to
OpenTelemetry (adapter/platform concern), structured log routing, metrics.

##### Access control

`trace.rs` is `pub` but is designated **adapter-only** by convention and documentation.
No Rust visibility modifier prevents function authors from calling `call_instrumented` or
`compensate_instrumented` directly; however, doing so would duplicate spans and emit
incorrect lifecycle events (a second `started` event for an already-running invocation).

The enforcement strategy is documentation and code review, not compiler enforcement.
A future option is to gate `trace.rs` behind a `adapter` Cargo feature flag
(disabled by default for function-author-facing builds); this is tracked as a
known limitation and should be evaluated if SDK misuse is observed in practice.

##### Span fields emitted

| Field | Source | Notes |
|-------|--------|-------|
| `invocation_id` | `ctx.invocation_id` | Always present |
| `function_id` | `ctx.function_id` | Always present |
| `function_version` | `ctx.function_version` | `call_instrumented` only |
| `tenant_id` | `ctx.tenant_id` | Always present |
| `attempt_number` | `ctx.attempt_number` | Always present |
| `correlation_id` | `ctx.correlation_id` | Always present |
| `trace_id` | `ctx.trace_id` | Recorded lazily; absent if `None` |
| `span_id` | `ctx.span_id` | Recorded lazily; absent if `None` |
| `original_workflow_invocation_id` | `input.original_workflow_invocation_id` | `compensate_instrumented` only |
| `compensation_trigger` | `input.trigger` | `compensate_instrumented` only |
| `failed_step_id` | `input.failed_step_id` | `compensate_instrumented` only |

### 3.3 API Contracts

#### Handler<I, O> Trait Contract

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-interface-handler-trait`

- **Type**: Rust async trait (`#[async_trait]`)
- **Technology**: `async-trait` 0.1
- **Stability**: unstable (0.x)

```
Handler<I, O>
  where I: DeserializeOwned + Send + 'static
        O: Serialize + Send + 'static
  Self: Send + Sync + 'static
  ─────────────────────────────────────────
  async fn call(
      &self,
      ctx: &Context,
      env: &dyn Environment,
      input: I,
  ) -> Result<O, ServerlessSdkError>
```

**Invariants**:
- `ctx` is immutable for the duration of `call`.
- `env` is populated before `call`; no async fetching inside. `get_config` and `get_secret`
  return `Option<&str>` that borrows from `&self` — every `Environment` implementation must
  own the string data (e.g., a `HashMap<String, String>`) and cannot lazily resolve values.
- Returning `Ok(O)` maps to `InvocationStatus::Succeeded`.
- Returning `Err(_)` maps to `InvocationStatus::Failed` (with retry if `Internal`).

#### WorkflowHandler<I, O> Trait Contract

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-interface-workflow-trait`

- **Type**: Rust async trait (`#[async_trait]`), supertrait of `Handler<I, O>`
- **Stability**: unstable (0.x)

```
WorkflowHandler<I, O>: Handler<I, O>
  ─────────────────────────────────────────
  async fn compensate(
      &self,
      ctx: &Context,
      env: &dyn Environment,
      input: CompensationInput,
  ) -> Result<(), ServerlessSdkError>
```

**Invariants**:
- `compensate` may be called more than once for the same `original_workflow_invocation_id`;
  implementations must be idempotent.
- Returning `Err(_)` transitions the original invocation to `dead_lettered`.

### 3.4 Internal Dependencies

| Dependency | Interface Used | Purpose |
|------------|----------------|---------|
| `serde` | `DeserializeOwned`, `Serialize` derives | Handler I/O type bounds |
| `serde_json` | `serde_json::Value` | Opaque JSON fields in `CompensationInput` |
| `thiserror` | `#[derive(thiserror::Error)]` | `ServerlessSdkError` `Display + Error` impl |
| `async-trait` | `#[async_trait]` | Stable async fn in `Handler` and `WorkflowHandler` |
| `tracing` | `info_span!`, `info!`, `error!`, `Instrument` | Timeline event emission in `trace.rs` only |

**Dependency Rules**:
- No circular dependencies (this is a leaf library).
- No cross-module type imports except through `lib.rs` re-exports.
- `trace.rs` may import from all other modules but no other module imports from `trace.rs`.

### 3.5 External Dependencies

This crate has no external system dependencies (no HTTP, no database, no message broker).
All external integration happens through adapter crates that depend on this crate.

### 3.6 Interactions & Sequences

#### Handler Invocation Flow

**ID**: `cpt-cf-serverless-sdk-core-seq-handler-call`

**Use cases**: `cpt-cf-serverless-sdk-core-usecase-impl-handler`

```mermaid
sequenceDiagram
    participant R as Serverless Runtime
    participant A as Adapter
    participant T as trace::call_instrumented
    participant H as Handler<I,O> impl

    R->>A: start_invocation(InvocationRecord)
    A->>A: populate Context from InvocationRecord
    A->>A: populate Environment (pre-fetch config + secrets)
    A->>A: deserialise params JSON → I
    A->>T: call_instrumented(handler, ctx, env, input)
    T->>T: create span "serverless.handler.call"
    T->>T: emit event "started"
    T->>H: call(ctx, env, input)
    H-->>T: Result<O, ServerlessSdkError>
    T->>T: emit event "succeeded" | "failed"
    T-->>A: Result<O, ServerlessSdkError>
    A->>A: serialise O → JSON or map Err → RuntimeErrorPayload
    A-->>R: InvocationResult
```

#### Compensation Flow

**ID**: `cpt-cf-serverless-sdk-core-seq-compensate`

**Use cases**: `cpt-cf-serverless-sdk-core-usecase-impl-compensation`

```mermaid
sequenceDiagram
    participant R as Serverless Runtime
    participant A as Adapter
    participant T as trace::compensate_instrumented
    participant W as WorkflowHandler<I,O> impl

    R->>A: start_invocation(compensation InvocationRecord)
    note over A: trigger="failure"|"cancellation"
    A->>A: deserialise params JSON → CompensationInput
    A->>A: populate Context, Environment
    A->>T: compensate_instrumented(handler, ctx, env, input)
    T->>T: create span "serverless.handler.compensate"
    T->>T: emit event "compensation_started"
    T->>W: compensate(ctx, env, input)
    W->>W: check idempotency on original_workflow_invocation_id
    W->>W: perform rollback using workflow_state_snapshot
    W-->>T: Result<(), ServerlessSdkError>
    T->>T: emit "compensation_completed" | "compensation_failed"
    T-->>A: Result<(), ServerlessSdkError>
    A-->>R: Ok → transition to compensated
    note over R: Err → transition to dead_lettered
```

### 3.7 Testability Architecture

The trait-based design ensures all components are independently testable without adapter
infrastructure.

**Mock boundaries:**

| Boundary | Test Double | Notes |
|----------|-------------|-------|
| `Environment` trait | Any `HashMap<String, String>`-backed impl | No credstore SDK, no async setup |
| `Handler<I, O>` | Direct invocation: `handler.call(&ctx, &env, input).await` | No adapter, no spawned tasks |
| `WorkflowHandler<I, O>` | Direct invocation: `handler.compensate(&ctx, &env, input).await` | Test idempotency with identical `original_workflow_invocation_id` |
| `Context` | Fully constructible in tests; set `deadline` to a past `Instant` to test expired-deadline paths | No runtime infrastructure |
| `trace.rs` | Any `tracing::Subscriber` (e.g., `tracing-subscriber` with test collector) | No SDK-specific subscriber |

**Test isolation approach:** Each module (`context.rs`, `environment.rs`, `error.rs`,
`handler.rs`, `workflow.rs`, `trace.rs`) is independently testable; no shared mutable
state across invocations. All SDK types are `Send + Sync`, compatible with parallel test runners.

**Testing strategy:**

| Level | Approach | Scope |
|-------|----------|-------|
| Unit | Per-module tests with HashMap-backed `Environment` mock and minimal `Context` | Trait compilation, error variant mapping, deadline helper behavior |
| Integration | Compile-only test: `impl Handler` + `impl WorkflowHandler` without any adapter crate | Verifies API contract compiles on stable 1.92.0 |
| Performance | Criterion benchmark for `call_instrumented` round-trip overhead | Verifies `nfr-low-overhead` threshold (one `Box<dyn Future>` + one span) |

### Database schemas & tables

**Not applicable.** This crate is a pure Rust library with no database, no persistence layer,
and no schema definitions. There are no SQL or NoSQL schemas, no ORM entities, and no
migration files owned by this module. All data structures are in-memory Rust types
defined in §3.1 (Domain Model).

### 3.8 Capacity, Cost, and Deployment Exclusions

The following capacity and cost planning domains are not applicable to this design and are
explicitly excluded:

| Domain | Disposition | Reasoning |
|--------|-------------|-----------|
| Capacity planning | N/A | Pure library crate; no runtime process, no user-facing endpoints, no data storage. Capacity is entirely determined by the consuming adapter and its runtime. |
| Cost estimation / budgeting | N/A | Pure library crate; no infrastructure provisioned, no compute resources allocated by this crate. Cost is an adapter/platform concern. |
| Deployment topology | N/A | Library crate with no deployment artifact; distributed as a Cargo crate, consumed at compile time. |

---

## 4. Additional Context

### Relationship to the Serverless Runtime Design

This crate is a downstream consumer of the Serverless Runtime's domain model
([DESIGN.md](../../serverless-runtime/docs/DESIGN.md)). Specifically:

- `Context` is a projection of `InvocationRecord` (§3.1 Core Types).
- `CompensationInput` is a projection of `CompensationContext`
  (`gts.x.core.serverless.compensation_context.v1~`, §3.1 WorkflowTraits).
- `ServerlessSdkError` variants map to `RuntimeErrorCategory` values (§3.1 Runtime Errors).
- `trace.rs` timeline events correspond to `TimelineEventType` values (§3.1 Additional Types).

This crate does **not** depend on the `serverless-runtime` module at the Cargo level.
The mapping is documented in this design and enforced by adapters at runtime.

### Comparison with Similar Solutions

| Solution | Handler model | Context model | Error model | Compensation |
|----------|---------------|---------------|-------------|--------------|
| **This crate** | `async trait Handler<I, O>` + `WorkflowHandler<I, O>: Handler<I, O>` | Concrete `Context` struct (platform-owned fields) | `#[non_exhaustive]` enum → `RuntimeErrorCategory` | `WorkflowHandler::compensate` (structured `CompensationInput`) |
| AWS Lambda Rust Runtime | `fn handler(event: E, ctx: Context) -> Result<R, E>` free-function or `tower::Service` | Concrete `lambda_runtime::Context` struct | `Box<dyn Error>` — opaque, no retry category | None — compensation is application-level |
| Temporal Rust SDK | `#[workflow]` proc-macro on async fn; activities as `#[activity]` async fn | `workflow::Context` injected via proc-macro | `ApplicationError` with explicit `non_retryable` flag | Step-level rollback via custom activity sequencing; no first-class saga trait |
| Cloudflare Workers (Rust via wasm) | `#[event(fetch)]` on async fn; `Request`/`Response` types | `Env` struct for bindings | `worker::Error` enum | None |
| Apache OpenWhisk Rust | Free function `fn main(args: Value) -> Value`; no trait | No context; caller metadata in args JSON | Return value discrimination (error key in JSON) | None |

**Key differentiators of this crate:**

- **Typed `RuntimeErrorCategory` mapping**: Unlike Lambda's opaque `Box<dyn Error>` or
  OpenWhisk's JSON key convention, `ServerlessSdkError` variants map deterministically to
  retry categories — the platform can make correct retry decisions without runtime inspection.
- **First-class compensation trait**: Unlike Temporal's SDK (which handles saga rollback
  via activity sequencing in the workflow body) or Lambda (no compensation concept),
  `WorkflowHandler::compensate` is a first-class, compiler-enforced obligation on every
  durable workflow.
- **No proc-macros, no code generation**: Unlike Temporal's `#[workflow]` / `#[activity]`
  macros, this crate uses plain traits and `#[async_trait]`. Function authors implement
  traits directly; no hidden code generation.
- **Adapter-agnostic by construction**: Unlike Lambda's SDK (AWS-specific) or Workers
  (Cloudflare-specific), this crate has no runtime-specific dependency; the same
  `Handler` implementation can run on any adapter without modification.

### Known Technical Debt

| Item | Nature | Migration Path |
|------|--------|----------------|
| `async-trait` dependency | One heap allocation (`Box<dyn Future>`) per `call`/`compensate` invocation; extra `#[async_trait]` annotation required on every `impl` block | Remove when RPITIT with `Send` bound is fully ergonomic on stable Rust. Migration is backward-compatible at the trait level. Tracked in `cpt-cf-serverless-sdk-core-adr-async-trait` Consequences §3. |

### Crate Naming Convention

Following the workspace `cf-<name>` convention:

| Artifact | Value |
|----------|-------|
| Directory | `modules/serverless-sdk/cyberfabric-serverless-sdk-core/` |
| Package name | `cf-serverless-sdk-core` |
| Lib name | `serverless_sdk_core` |
| Import | `use serverless_sdk_core::...` |

---

## 5. Non-Applicable Domains

The following checklist domains are not applicable to this DESIGN. Absence of content
in these areas is deliberate, not an omission.

| Domain | Checklist Item | Disposition | Reasoning |
|--------|---------------|-------------|-----------|
| SEC — Authentication | SEC-DESIGN-001 | N/A | No user sessions, no HTTP endpoints, no credential management at the SDK layer. |
| SEC — Authorization | SEC-DESIGN-002 | N/A | No permission model; all actors are trusted internal platform developers. |
| SEC — Data Protection | SEC-DESIGN-003 | N/A | SDK passes data as opaque blobs (`I`, `O`, `serde_json::Value`); no PII stored or transmitted. |
| SEC — Security Boundaries | SEC-DESIGN-004 | N/A | Pure library; no network boundary, no process boundary, no trust zone separation required. |
| SEC — Threat Modeling | SEC-DESIGN-005 | N/A | Pure library with no attack surface; no network exposure, no credential management, no data storage. |
| SEC — Audit Logging | SEC-DESIGN-006 | Addressed via §3.2 `component-trace` | Invocation lifecycle tracing events are emitted by `trace.rs`; security audit beyond invocation events is adapter/runtime concern. |
| DATA — Data Stores | DATA-DESIGN-001 | N/A | No data stores; all state is in-memory per invocation. See `Database schemas & tables` in §3. |
| DATA — Data Integrity | DATA-DESIGN-002 | N/A | No persistent data; no referential integrity requirements. Values are passed through as opaque blobs. |
| DATA — Data Governance | DATA-DESIGN-003 | N/A | No data ownership, stewardship, or retention; data classification is the adapter/runtime concern. |
| OPS — Deployment Topology | OPS-DESIGN-001 | N/A | Library crate with no deployment artifact. See §3.8. |
| OPS — IaC | OPS-DESIGN-003 | N/A | No infrastructure provisioned by this crate; no Terraform, K8s, Docker, or CI/CD resources owned here. |
| OPS — SLO Targets | OPS-DESIGN-004 | N/A | Library crate; no uptime SLO. Per-invocation overhead is bounded by `nfr-low-overhead` (§1.2), not an SLO. |
| COMPL — Regulatory Requirements | COMPL-DESIGN-001 | N/A | Internal Rust library; no GDPR, HIPAA, PCI DSS, or other regulatory obligations. |
| COMPL — Privacy by Design | COMPL-DESIGN-002 | N/A | Internal developer tooling; no end-user data collection, no PII flows. |
| UX — User-Facing Architecture | UX-DESIGN-001 | N/A | No end-user UI; developer SDK only. Developer ergonomics addressed via trait design and `nfr-api-docs`. |
| PERF — Performance Architecture | PERF-DESIGN-001–004 | N/A | Pure library with no runtime process, database, or network I/O. Per-invocation overhead bounded by `nfr-low-overhead` (§1.2 NFR Allocation); caching, scalability, and resource pooling are adapter/runtime concerns. |
| REL — Error Handling | REL-DESIGN-002 | Addressed via §3.1, §3.3 | Error classification (`ServerlessSdkError` → `RuntimeErrorCategory` mapping) and dead-letter routing documented in §3.1 error mapping table and §3.3 trait contract invariants. |
| REL — Data Consistency / Saga | REL-DESIGN-003 | Addressed via §3.2, §3.3 | Compensating transaction pattern documented in `component-workflow`; idempotency invariant stated in `WorkflowHandler` contract (§3.3). |
| REL — Fault Tolerance, Recovery, Resilience | REL-DESIGN-001, 004, 005 | N/A | Pure library; no infrastructure to make fault-tolerant or recover. Retry policies, failover, and circuit breakers are runtime/adapter concerns. |
| INT — Integration Architecture | INT-DESIGN-001, 003, 004 | N/A | No external system integrations, event buses, or API gateway. Adapter contract integration documented in §3.3 and §3.5. |
| INT — External System Integration | INT-DESIGN-002 | Addressed via §3.5 | This crate has no external system dependencies; all external integration happens through adapter crates (§3.5). |

---

## 6. Traceability

- **PRD**: [PRD.md](./PRD.md)
- **ADRs**: [ADR/](./ADR/)
- **Source**: [cyberfabric-serverless-sdk-core/src/](../cyberfabric-serverless-sdk-core/src/)
- **Serverless Runtime DESIGN**: [modules/serverless-runtime/docs/DESIGN.md](../../serverless-runtime/docs/DESIGN.md)
- **Serverless Runtime Rust Types**: [modules/serverless-runtime/docs/DESIGN_RUST_TYPES.md](../../serverless-runtime/docs/DESIGN_RUST_TYPES.md)
