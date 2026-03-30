<!--
Created: 2026-03-30 by Constructor Tech
Updated: 2026-03-30 by Constructor Tech
-->

# PRD — CyberFabric Serverless SDK Core


<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Module-Specific Environment Constraints](#31-module-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 FunctionHandler Trait](#51-functionhandler-trait)
  - [5.2 WorkflowHandler Trait](#52-workflowhandler-trait)
  - [5.3 Invocation Context](#53-invocation-context)
  - [5.4 Environment](#54-environment)
  - [5.5 Error Model](#55-error-model)
  - [5.6 Tracing Instrumentation](#56-tracing-instrumentation)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Module-Specific NFRs](#61-module-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Traceability](#13-traceability)

<!-- /toc -->

<!--
=============================================================================
PRODUCT REQUIREMENTS DOCUMENT (PRD)
=============================================================================
PURPOSE: Define WHAT the system must do and WHY — business requirements,
functional capabilities, and quality attributes.

SCOPE:
  ✓ Business goals and success criteria
  ✓ Actors (users, systems) that interact with this module
  ✓ Functional requirements (WHAT, not HOW)
  ✓ Non-functional requirements (quality attributes, SLOs)
  ✓ Scope boundaries (in/out of scope)
  ✓ Assumptions, dependencies, risks

NOT IN THIS DOCUMENT (see other templates):
  ✗ Technical architecture, design decisions → DESIGN.md
  ✗ Why a specific technical approach was chosen → ADR/
  ✗ Detailed implementation flows, algorithms → features/

STANDARDS ALIGNMENT:
  - IEEE 830 / ISO/IEC/IEEE 29148:2018 (requirements specification)
  - ISO/IEC 15288 / 12207 (requirements definition)
=============================================================================
-->
## 1. Overview

### 1.1 Purpose

`cf-serverless-sdk-core` is the engine-agnostic, stable Rust library at the heart
of the CyberFabric Serverless SDK. It provides the abstractions that function and
workflow authors implement to register logic with the Serverless Runtime, without
coupling to any specific execution engine (Temporal, Starlark, cloud FaaS, or
any future adapter).

The crate defines a minimal, opinionated set of traits and types that covers the
complete handler authoring contract: receiving invocation context, accessing
configuration and secrets, returning typed outputs, implementing durable workflow
compensation, and emitting structured observability events. Adapter crates build
on this foundation without ever modifying it.

### 1.2 Background / Problem Statement

The CyberFabric Serverless Runtime
(`cpt-cf-serverless-runtime-principle-impl-agnostic`) is designed to support
multiple execution engines through a pluggable adapter model. Without a shared,
engine-agnostic SDK core, each adapter would define its own handler contract,
forcing function and workflow authors to rewrite their logic when changing adapters,
and preventing the platform from enforcing a consistent authoring contract, error
model, or observability surface.

A stable, engine-agnostic SDK core solves this by defining the contract once.
Adapters implement it; function and workflow authors depend on it. This enables
adapter portability, consistent error classification, uniform observability, and
a single authoring mental model regardless of the underlying runtime technology.

### 1.3 Goals (Business Outcomes)

_Baseline: module is new (no prior implementation). All targets apply at first stable release (v0.1.0)._

- **FunctionHandler portability**: Function and workflow authors can implement handlers that compile
  and work unchanged across any CyberFabric execution adapter.
  _Target: zero adapter-specific changes required to port a conformant handler between adapters, verified by a shared test suite._
- **Error categorisation completeness**: The SDK error model maps unambiguously to runtime
  `RuntimeErrorCategory` values, enabling correct retry and dead-letter routing without
  adapter-specific error handling.
  _Target: 100% of `ServerlessSdkError` variants carry a documented `RuntimeErrorCategory` mapping; no unmapped (`Unknown`) fallback case._
- **Observability zero-overhead for consumers**: Adapters can instrument every invocation
  with structured tracing spans and timeline events without requiring SDK consumers to add
  any observability code.
  _Target: a conformant `FunctionHandler` implementation contains zero `tracing` imports, verified by compile-time import audit in CI._
- **Toolchain stability**: The crate compiles on stable Rust with zero unsafe code.
  _Target: `cargo check` passes on stable 1.92.0; `cargo clippy` reports zero warnings; `cargo deny` reports zero engine-specific transitive dependencies. All enforced on every CI run._

### 1.4 Glossary

| Term | Definition |
|------|------------|
| **FunctionHandler** | A Rust type that implements the `FunctionHandler<I, O>` trait to service function invocations. |
| **WorkflowHandler** | A `FunctionHandler` that additionally implements compensation for durable workflow rollback. |
| **Context** | Read-only invocation metadata (ID, tenant, attempt, deadline) derived from `InvocationRecord`. |
| **Environment** | Abstraction over configuration and secret access for a handler invocation. |
| **Compensation** | The rollback contract for durable workflows in CyberFabric. Two layers: function-level compensation is implemented by the function author via `WorkflowHandler::compensate` and invoked by the platform as a standard invocation with a `CompensationInput` payload; step-level compensation (sub-step rollback within a workflow execution) is owned by the executor, not the SDK. |
| **Adapter** | A CyberFabric module that implements `ServerlessRuntime` and drives handlers via this SDK. |
| **GTS ID** | An opaque Global Type System identifier string; the SDK carries these as `String` without interpretation. |
| **Timeline Event** | A structured tracing event mapping to `InvocationTimelineEvent` in the runtime domain. |

---

## 2. Actors

### 2.1 Human Actors

#### Function Author

**ID**: `cpt-cf-serverless-sdk-core-actor-fn-author`

- **Role**: A platform developer who implements `FunctionHandler<I, O>` or `WorkflowHandler<I, O>`
  to register custom function or workflow logic with the Serverless Runtime.
- **Needs**: A stable, ergonomic Rust trait contract with typed input/output,
  access to invocation context, config/secret access, and a clear error model.

#### Adapter Developer

**ID**: `cpt-cf-serverless-sdk-core-actor-adapter-dev`

- **Role**: A platform developer building an adapter crate (e.g., Starlark, Temporal).
  Drives handler execution by calling the SDK traits and using the `trace` module
  for instrumentation.
- **Needs**: Stable, well-documented traits they can drive; instrumentation utilities
  that emit consistent timeline events; clear contracts for how `Context` and
  `Environment` are populated.

### 2.2 System Actors

#### Serverless Runtime

**ID**: `cpt-cf-serverless-sdk-core-actor-runtime`

- **Role**: The CyberFabric Serverless Runtime module that owns `InvocationRecord`,
  manages invocation lifecycle, and routes invocations to adapters. The SDK core
  is an upstream dependency of adapter crates that the runtime drives.
- **Needs**: Stable trait contracts that adapters can drive without coupling to engine
  internals; a consistent error categorisation interface mapping `ServerlessSdkError`
  variants to `RuntimeErrorCategory` for retry and dead-letter routing decisions;
  structured invocation metadata (`Context`) that maps unambiguously from
  `InvocationRecord` fields.

---

## 3. Operational Concept & Environment

### 3.1 Module-Specific Environment Constraints

- This crate is a library (`[lib]`). It has no runtime binary and no database.
- It must compile on **stable Rust** (workspace `rust-version = "1.92.0"`).
- It must contain **no unsafe code** (enforced by workspace `unsafe_code = "forbid"`).
- It must have **no engine-specific dependencies** (no `temporal-sdk`, `starlark`,
  or similar crates) — ever.
- **Developer experience target** (UX-PRD-001): Target users are Rust developers with
  intermediate async experience (familiar with `async/await`, trait implementations,
  and `serde`). A Function Author MUST be able to implement a conformant `FunctionHandler`
  using only the crate's `rustdoc` and this PRD, without consulting adapter internals
  or engine documentation.

---

## 4. Scope

### 4.1 In Scope

- `FunctionHandler<I, O>` trait for stateless function invocations.
- `WorkflowHandler<I, O>` trait extending `FunctionHandler` with compensation.
- `Context` type populated from `InvocationRecord` metadata.
- `Environment` trait for synchronous config and secret access.
- `ServerlessSdkError` typed error enum with `RuntimeErrorCategory` mapping.
- `CompensationInput` and `CompensationTrigger` types for saga compensation.
- `trace` module with adapter-facing instrumentation utilities that emit
  `tracing` spans and timeline events.

### 4.2 Out of Scope

- Proc-macro crate (`cf-serverless-sdk-macros`) — future work.
- Adapter crates (`cf-serverless-sdk-adapter-*`) — future work.
- Testing utilities crate (`cf-serverless-sdk-testing`) — future work.
- Workspace `Cargo.toml` member registration — handled at integration time.
- Cypilot `artifacts.toml` registration — handled at integration time.
- GTS schema validation or GTS chain parsing.
- `InvocationStatus` state machine — owned by the runtime.
- `TenantRuntimePolicy`, `Schedule`, `Trigger`, `Webhook` — owned by the runtime.
- Any retry policy logic — runtime concern; SDK only exposes `attempt_number`.

---

## 5. Functional Requirements

### 5.1 FunctionHandler Trait

#### Async Typed FunctionHandler

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-fr-handler-trait`

The crate MUST provide an `async` trait `FunctionHandler<I, O>` where `I: DeserializeOwned + Send + 'static`
and `O: Serialize + Send + 'static`, with a single method `call(&self, ctx: &Context, env: &dyn Environment, input: I) -> Result<O, ServerlessSdkError>`.

- **Rationale**: Provides the typed, adapter-neutral authoring contract for all stateless functions.
- **Actors**: `cpt-cf-serverless-sdk-core-actor-fn-author`, `cpt-cf-serverless-sdk-core-actor-adapter-dev`

#### FunctionHandler Send+Sync Bound

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-fr-handler-send-sync`

`FunctionHandler<I, O>` MUST require `Self: Send + Sync + 'static` so handlers can be
stored in `Arc<dyn ...>` and dispatched across async tasks by adapters.

- **Rationale**: Adapters run on multi-threaded async runtimes; handler instances must be safely shared.
- **Actors**: `cpt-cf-serverless-sdk-core-actor-adapter-dev`

### 5.2 WorkflowHandler Trait

#### Workflow Compensation

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-fr-workflow-handler-trait`

The crate MUST provide an `async` trait `WorkflowHandler<I, O>` that extends `FunctionHandler<I, O>`
with a `compensate(&self, ctx: &Context, env: &dyn Environment, input: CompensationInput) -> Result<(), ServerlessSdkError>` method.

- **Rationale**: Implements the function-level compensation layer of the two-layer saga model
  (`cpt-cf-serverless-runtime-fr-advanced-patterns` BR-133).
- **Actors**: `cpt-cf-serverless-sdk-core-actor-fn-author`

#### CompensationInput

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-fr-compensation-input`

The crate MUST provide a `CompensationInput` struct that carries all fields from the runtime's
`CompensationContext` (`gts.x.core.serverless.compensation_context.v1~`) required for
idempotent rollback: `trigger`, `original_workflow_invocation_id`, `failed_step_id`,
`failed_step_error` (typed: `error_type`, `message`, `error_metadata`),
`workflow_state_snapshot`, `timestamp`, `function_id`,
`original_input`, `tenant_id`, `correlation_id`, `started_at`.

Field names and optionality MUST match the runtime's `CompensationContext` schema:
`correlation_id` and `started_at` are `Option<String>` (optional in the runtime schema).

- **Rationale**: Compensation handlers need the full context to perform idempotent rollback
  without accessing the runtime domain types.
- **Actors**: `cpt-cf-serverless-sdk-core-actor-fn-author`

### 5.3 Invocation Context

#### Context Populated from InvocationRecord

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-fr-context`

The crate MUST provide a `Context` struct with the following fields:

- From `InvocationRecord`: `invocation_id`, `function_id`, `function_version`, `tenant_id`.
- From `InvocationObservability`: `correlation_id: String` (required in
  `InvocationObservability`), `trace_id: Option<String>`, `span_id: Option<String>`.
- Adapter-supplied: `attempt_number: u32` (1-indexed; the adapter tracks retry count
  independently — the runtime's `InvocationRecord` does not expose an attempt counter,
  though retry count is tracked at the persistence layer).
- Computed: `deadline: Option<Instant>` derived from `FunctionLimits.timeout_seconds` at
  invocation start.

- **Rationale**: Maps the runtime's invocation record to the minimal SDK surface handlers need.
  Follows `cpt-cf-serverless-runtime-principle-impl-agnostic`: no engine-specific fields.
- **Actors**: `cpt-cf-serverless-sdk-core-actor-fn-author`

#### Deadline Helpers

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-fr-deadline-helpers`

`Context` MUST provide `is_deadline_exceeded() -> bool` and `remaining_time() -> Option<Duration>`
helper methods so handlers can detect and respond to deadline expiry before forced termination.

- **Rationale**: Enables handlers to self-terminate cleanly (returning `ServerlessSdkError::Timeout`)
  rather than being killed mid-operation, which could leave partial state.
- **Actors**: `cpt-cf-serverless-sdk-core-actor-fn-author`

### 5.4 Environment

#### Config and Secret Access

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-fr-environment-trait`

The crate MUST provide an `Environment` trait with `get_config(key: &str) -> Option<&str>`
and `get_secret(key: &str) -> Option<&str>`. Adapters supply the implementation populated
before each invocation.

- **Rationale**: Provides engine-agnostic access to function configuration and credentials
  without coupling to credstore APIs or async resolution inside handler logic.
- **Actors**: `cpt-cf-serverless-sdk-core-actor-fn-author`, `cpt-cf-serverless-sdk-core-actor-adapter-dev`

### 5.5 Error Model

#### ServerlessSdkError Variants

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-fr-error-model`

The crate MUST provide a `ServerlessSdkError` enum with at minimum the following variants
and their documented `RuntimeErrorCategory` mappings:

| Variant | `RuntimeErrorCategory` |
|---------|------------------------|
| `UserError(String)` | `NonRetryable` |
| `InvalidInput(String)` | `NonRetryable` |
| `Timeout` | `Timeout` |
| `NotSupported(String)` | `NonRetryable` |
| `Internal(String)` | `Retryable` |

The enum MUST be `#[non_exhaustive]` to allow future variants without breaking downstream.

Variant semantics: `InvalidInput` is for structural or type constraint violations (checked
before side effects); `UserError` is for business-logic rejections (checked after domain
rules are evaluated). Both map to `NonRetryable` — the distinction is for observability and
caller-facing error messaging, not for retry behaviour.

The runtime's `RuntimeErrorCategory` also defines `ResourceLimit` and `Canceled`. These are
**adapter-only categories** — handlers never produce them. `ResourceLimit` is signalled by the
adapter when tenant quotas or resource limits are exceeded before or during invocation.
`Canceled` is applied by the runtime when an invocation is externally canceled. The SDK error
model intentionally excludes both because they originate outside handler code.

- **Rationale**: Unambiguous mapping from handler errors to runtime retry and dead-letter routing.
- **Actors**: `cpt-cf-serverless-sdk-core-actor-fn-author`, `cpt-cf-serverless-sdk-core-actor-adapter-dev`

### 5.6 Tracing Instrumentation

#### Adapter-Facing Timeline Instrumentation

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-fr-trace-module`

The crate MUST provide a `trace` module with `call_instrumented` and `compensate_instrumented`
functions that wrap handler invocations in structured `tracing` spans and emit lifecycle events
mapping to `InvocationTimelineEvent` variants (`started`, `succeeded`, `failed`,
`compensation_started`, `compensation_completed`, `compensation_failed`).

- **Rationale**: Enables adapters to emit consistent timeline events without requiring SDK
  consumers to add any observability code.
- **Actors**: `cpt-cf-serverless-sdk-core-actor-adapter-dev`

#### No Consumer-Visible Tracing

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-fr-no-consumer-tracing`

`FunctionHandler::call` and `WorkflowHandler::compensate` MUST NOT emit any `tracing` events
or spans directly. All instrumentation is contained in the `trace` module and is
invisible to SDK consumers.

- **Rationale**: FunctionHandler implementations remain clean and free of platform-specific observability
  wiring; adapters control the observability boundary.
- **Actors**: `cpt-cf-serverless-sdk-core-actor-fn-author`

---

## 6. Non-Functional Requirements

### 6.1 Module-Specific NFRs

#### No Engine-Specific Dependencies

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-nfr-no-engine-deps`

The crate MUST NOT introduce any direct or transitive dependency on engine-specific crates
(`temporal-sdk`, `starlark`, any cloud FaaS SDK, or similar) at any point in its lifecycle.

- **Threshold**: Zero engine-specific crates in the dependency tree.
- **Rationale**: Enforces `cpt-cf-serverless-runtime-principle-impl-agnostic`; prevents
  accidental coupling that would break adapter portability.
- **Verification Method**: Automated `cargo tree` audit in CI; dependency policy review on every PR.

#### Stable Rust Compatibility

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-nfr-stable-rust`

The crate MUST compile without errors on the workspace minimum stable Rust version
(`rust-version = "1.92.0"`) with no nightly features.

- **Threshold**: `cargo check` passes on stable 1.92.0.
- **Rationale**: Function and workflow authors should not need to manage toolchain versions.

#### Zero Unsafe Code

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-nfr-no-unsafe`

The crate MUST contain no `unsafe` blocks (enforced by `unsafe_code = "forbid"` in the
workspace lint configuration).

- **Threshold**: Zero `unsafe` blocks; workspace lint enforces this.
- **Rationale**: Safety guarantee for handler authors; no soundness risk from SDK internals.

#### Low Per-Invocation Overhead

- [ ] `p2` - **ID**: `cpt-cf-serverless-sdk-core-nfr-low-overhead`

The SDK's contribution to per-handler-invocation overhead MUST be minimal and predictable
for production async workloads.

- **Threshold**: `call_instrumented` MUST NOT introduce blocking I/O or synchronous
  computation beyond one `Box<dyn Future>` allocation (from the async trait mechanism;
  see DESIGN.md for rationale) and one
  `tracing` span emission per call. No additional heap allocations on the hot path beyond
  those two.
- **Rationale**: The SDK is on the critical invocation path for all adapters; latency overhead
  accumulates across high-frequency workloads. The `async-trait` boxing cost is accepted
  (see §12 Risks); additional overhead is not.
- **Verification Method**: Criterion benchmark measuring `call_instrumented` round-trip
  overhead; reviewed before each release.

#### Public API Documentation

- [ ] `p2` - **ID**: `cpt-cf-serverless-sdk-core-nfr-api-docs`

All public types, traits, and functions MUST have `rustdoc` documentation covering
purpose, usage, and any invariants or panics.

- **Threshold**: `cargo doc --no-deps` produces zero missing-documentation warnings;
  enforced by `#![deny(missing_docs)]` in CI.
- **Rationale**: Function Authors and Adapter Developers must be able to understand and
  implement the SDK contract from the documentation alone, without consulting DESIGN.md
  or engine internals. Aligns with UX-PRD-001 developer-experience target.
- **Verification Method**: `cargo doc --no-deps` in CI; zero missing-doc warnings.

#### Authoring Ergonomics

- [ ] `p2` - **ID**: `cpt-cf-serverless-sdk-core-nfr-authoring-ergonomics`

Function and workflow authors MUST be able to implement `FunctionHandler<I, O>` and
`WorkflowHandler<I, O>` using plain `async fn` syntax with no explicit lifetime
annotations, no manual `Pin<Box<dyn Future>>` return types, and no boilerplate beyond
the `impl` block itself.

- **Threshold**: Any `impl FunctionHandler` or `impl WorkflowHandler` block that requires
  explicit lifetime parameters on the `call` or `compensate` method signature is a
  violation of this NFR.
- **Rationale**: The SDK's value proposition is that function authors focus on business
  logic, not Rust async machinery. If implementing the handler trait requires knowledge
  of RPITIT lifetime elision rules or manual `Future` pinning, the trait imposes an
  unjustified cognitive burden on the primary audience.
- **Verification Method**: SDK examples and integration tests must compile with
  `async fn call(...)` / `async fn compensate(...)` syntax; CI fails if any handler
  impl requires explicit `impl Future` or lifetime annotation on the method signature.

### 6.2 NFR Exclusions

The following checklist domains are explicitly not applicable to this artifact. Absence of
requirements in these areas is deliberate, not an omission.

| Domain | Disposition | Reasoning |
|--------|-------------|-----------|
| Database / persistence | N/A | Pure library; no persistence layer. |
| REST API / HTTP | N/A | No HTTP endpoints; all interfaces are Rust traits. |
| Deployment topology (OPS-PRD-001) | N/A | Library crate; no deployment artifact. |
| Scalability / throughput / geographic distribution (PERF-PRD-002, PERF-PRD-003) | N/A | Pure library with no runtime process, network activity, or data storage. Throughput is determined by the adapter, not by this crate. |
| Availability / uptime SLOs (REL-PRD-001) | N/A | Library crate; availability is determined by the consuming process (adapter). |
| Disaster recovery / RPO / RTO (REL-PRD-002) | N/A | No stateful data; pure library with no persistence to recover. |
| Authentication requirements (SEC-PRD-001) | N/A | No user-facing sessions, no HTTP endpoints, no credential management at the SDK layer. Auth to secret stores is the adapter's responsibility. |
| Authorization / RBAC (SEC-PRD-002) | N/A | No permission model; all actors are trusted internal platform developers. Access control is the adapter/runtime concern. |
| PII / data classification (SEC-PRD-003) | N/A | The SDK does not store, transmit, or inspect data. `CompensationInput` fields and `Environment` secrets are opaque blobs; data classification is the adapter/runtime concern. |
| Audit logging as security concern (SEC-PRD-004) | Addressed via §5.6 | Invocation lifecycle tracing events are emitted per §5.6 (trace module). Security audit logging beyond invocation events is the adapter/runtime concern. |
| Privacy by design (SEC-PRD-005) | N/A | Internal developer tooling; no end-user PII collection. |
| Operational safety / fail-safe (SAFE-PRD-001, SAFE-PRD-002) | N/A | Pure library; no safety-critical operations, no physical systems interface, no hazards. |
| Monitoring / alerting / log retention (OPS-PRD-002) | N/A | Pure library; monitoring configuration is the adapter/runtime concern. The `trace` module provides the observability hooks; consumers configure retention. |
| UX accessibility — WCAG (UX-PRD-002) | N/A | No end-user UI; developer SDK only. |
| UX internationalization (UX-PRD-003) | N/A | No UI or user-facing strings. Error message strings are English-only; acceptable for internal developer tooling. |
| UX inclusivity (UX-PRD-005) | N/A | Developer SDK; no diverse end-user population considerations. |
| Support SLA / support tiers (MAINT-PRD-002) | N/A | Internal library; no external support tier. Issues tracked via the CyberFabric project repository. |
| Regulatory compliance — GDPR / HIPAA / PCI DSS (COMPL-PRD-001) | N/A | Internal Rust library; no regulatory obligations apply directly to this crate. |
| Industry certification standards (COMPL-PRD-002) | N/A | No formal certification required for an internal SDK crate. IEEE/ISO standards referenced in preamble are informational only. |
| Legal / ToS / consent requirements (COMPL-PRD-003) | N/A | Internal tool; no ToS, privacy policy, or consent flows. |
| Data ownership / stewardship (DATA-PRD-001) | N/A | The SDK passes data through as opaque blobs; ownership is the adapter/runtime concern. |
| Data quality requirements (DATA-PRD-002) | N/A | Pure library; no data storage or quality management. |
| Data lifecycle / retention / purge (DATA-PRD-003) | N/A | Pure library; no data persistence, retention, or archival. |

---

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Core Traits

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-interface-core-traits`

- **Type**: Rust traits (`FunctionHandler<I, O>`, `WorkflowHandler<I, O>`, `Environment`)
- **Stability**: unstable (0.x until adapters and macros stabilise)
- **Description**: The primary authoring contract for function and workflow implementations.
- **Breaking Change Policy**: Minor version bump for additive changes (new optional methods
  with defaults); major version bump for any breaking trait signature change.

#### Core Types

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-interface-core-types`

- **Type**: Rust structs/enums (`Context`, `CompensationInput`, `CompensationTrigger`,
  `ServerlessSdkError`)
- **Stability**: unstable (0.x)
- **Description**: Stable value types shared between adapters and handler authors.
- **Breaking Change Policy**: `#[non_exhaustive]` on enums allows new variants without
  a major bump; adding required struct fields is a breaking change.

#### Trace Utilities

- [x] `p2` - **ID**: `cpt-cf-serverless-sdk-core-interface-trace`

- **Type**: Rust module (`trace`) with free functions
- **Stability**: unstable
- **Description**: Adapter-facing instrumentation wrappers. Not intended for direct use
  by function authors.
- **Breaking Change Policy**: Minor version bump for signature changes.

### 7.2 External Integration Contracts

#### Serverless Runtime InvocationRecord

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-contract-invocation-record`

- **Direction**: required from adapter (adapter populates `Context` from `InvocationRecord`)
- **Protocol/Format**: Rust struct mapping documented in `DESIGN.md §3.1`
- **Compatibility**: `Context` field additions are backward-compatible; removals are breaking.

#### Serverless Runtime CompensationContext

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-contract-compensation-context`

- **Direction**: required from adapter (adapter populates `CompensationInput` from
  `gts.x.core.serverless.compensation_context.v1~`)
- **Protocol/Format**: JSON → `CompensationInput` mapping documented in `DESIGN.md §3.1`
- **Compatibility**: `CompensationInput` is `#[non_exhaustive]`; new fields are backward-compatible.

---

## 8. Use Cases

#### Author Implements a FunctionHandler

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-usecase-impl-handler`

**Actor**: `cpt-cf-serverless-sdk-core-actor-fn-author`

**Preconditions**:
- `cf-serverless-sdk-core` is a Cargo dependency of the function crate.
- The function's `IOSchema.params` and `IOSchema.returns` are known.

**Main Flow**:
1. Author defines Rust structs for input `I` and output `O` with `serde` derives.
2. Author implements `FunctionHandler<I, O>` on their handler struct.
3. `call` receives a typed `I`, reads `ctx` for invocation metadata and `env` for
   config/secrets, and returns `Result<O, ServerlessSdkError>`.
4. Adapter discovers the handler, deserialises params into `I`, calls
   `trace::call_instrumented`, receives `Result<O, _>`, and persists the result.

**Postconditions**:
- `InvocationRecord.result` contains the serialised `O`.
- A `serverless.handler.call` span with `succeeded` event is emitted.

**Alternative Flows**:
- **FunctionHandler returns `Err(UserError)`**: `InvocationRecord.error` is set with
  `RuntimeErrorCategory::NonRetryable`; no retry is attempted.
- **FunctionHandler returns `Err(Internal)`**: `RuntimeErrorCategory::Retryable`; runtime
  may retry per the function's `RetryPolicy`.
- **Deadline exceeded**: FunctionHandler calls `ctx.is_deadline_exceeded()`, returns
  `Err(Timeout)`; mapped to `RuntimeErrorCategory::Timeout`.

#### Adapter Developer Wires SDK into an Adapter Crate

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-usecase-wire-adapter`

**Actor**: `cpt-cf-serverless-sdk-core-actor-adapter-dev`

**Preconditions**:
- `cf-serverless-sdk-core` is a Cargo dependency of the adapter crate.
- The adapter has access to an `InvocationRecord` and the runtime's `CompensationContext`.

**Main Flow**:
1. Adapter constructs a `Context` by mapping fields from `InvocationRecord` and `InvocationObservability`.
2. Adapter implements the `Environment` trait, pre-fetching config and secret values before invocation.
3. Adapter resolves the handler instance (`Arc<dyn FunctionHandler<I, O>>`) for the requested function.
4. Adapter calls `trace::call_instrumented(handler, &ctx, &env, input)` to dispatch the invocation with automatic tracing.
5. Adapter receives `Result<O, ServerlessSdkError>`, maps the error variant to `RuntimeErrorCategory`, and persists the result in `InvocationRecord`.

**Postconditions**:
- `InvocationRecord` is updated with the serialised result or mapped error category.
- Lifecycle span events are emitted via the `tracing` subscriber without any handler-side code.

**Alternative Flows**:
- **Compensation trigger**: Adapter constructs `CompensationInput` from `CompensationContext` and calls
  `trace::compensate_instrumented` on a `WorkflowHandler` instance.

---

#### Author Implements Workflow Compensation

- [x] `p1` - **ID**: `cpt-cf-serverless-sdk-core-usecase-impl-compensation`

**Actor**: `cpt-cf-serverless-sdk-core-actor-fn-author`

**Preconditions**:
- Author has a `WorkflowHandler<I, O>` implementation.
- Workflow failed or was canceled; `CompensationInput` is available.

**Main Flow**:
1. Adapter calls `trace::compensate_instrumented(handler, ctx, env, input)`.
2. `compensate` checks `input.original_workflow_invocation_id` for idempotency.
3. Reads `input.failed_step_id` to determine rollback scope.
4. Reads `input.workflow_state_snapshot` for completed-step outputs needed for reversal.
5. Performs rollback operations (e.g., refund payment, release inventory).
6. Returns `Ok(())`.

**Postconditions**:
- Original invocation transitions to `compensated`.
- A `serverless.handler.compensate` span with `compensation_completed` event is emitted.

**Alternative Flows**:
- **compensation returns `Err(_)`**: Original invocation transitions to `dead_lettered`.

---

## 9. Acceptance Criteria

- [ ] A minimal `impl FunctionHandler` compiles and runs without referencing any adapter crate.
- [ ] A minimal `impl WorkflowHandler` compiles; the `compensate` method receives a
      `CompensationInput` with all fields from the `CompensationContext → CompensationInput`
      mapping table in DESIGN.md §3.1.
- [ ] A `HashMap<String, String>`-backed `Environment` implementation satisfies the
      `Environment` trait and is usable in handler unit tests without any platform
      infrastructure or async executor.
- [ ] Every `ServerlessSdkError` variant has a documented `RuntimeErrorCategory` mapping
      in its `rustdoc`; `cargo doc --no-deps` produces zero missing-doc warnings.
- [ ] `trace::call_instrumented` emits `started`, `succeeded`, and `failed` lifecycle
      span events for handler invocations without any `tracing` import in the handler
      implementation.
- [ ] `trace::compensate_instrumented` emits `compensation_started`, `compensation_completed`,
      and `compensation_failed` lifecycle span events without any `tracing` import in the
      workflow handler implementation.

---

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| Serialisation / deserialisation | Input/output serialisation for `FunctionHandler` type params and `CompensationInput` fields | p1 |
| JSON value type | Opaque JSON fields in `CompensationInput` (`workflow_state_snapshot`, etc.) | p1 |
| Error derivation | Ergonomic `Display + std::error::Error` derivation for `ServerlessSdkError` | p1 |
| Stable async trait mechanism | Async fn in trait definitions for `FunctionHandler` and `WorkflowHandler` (see DESIGN.md for rationale) | p1 |
| Structured tracing | Span emission in `trace` module | p1 |
| Serverless Runtime DESIGN | Defines `InvocationRecord`, `CompensationContext`, `RuntimeErrorCategory` | p1 |

---

## 11. Assumptions

- Adapters are responsible for deserialising `InvocationRecord.params` into `I` before
  calling `FunctionHandler::call`; this crate does not perform that deserialisation.
- Adapters pre-fetch all required config and secret values before calling the handler;
  `Environment` is synchronous by design.
- GTS type ID strings (`function_id`, `error_type_id`, etc.) are treated as opaque
  identifiers; this crate never parses or validates them.
- `attempt_number` is provided by the adapter (which tracks retry count independently)
  and starts at 1 for the initial attempt. The runtime's `InvocationRecord` does not
  expose an attempt counter field; retry count is tracked at the persistence layer.

---

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Trait signature changes break downstream handlers | High — any `FunctionHandler` impl stops compiling | Keep crate at 0.x; communicate breaking changes via semver major bump |
| Engine-specific dep accidentally introduced via transitive pull | High — violates `cpt-cf-serverless-sdk-core-nfr-no-engine-deps` | `cargo deny` CI gate; minimal dependency surface |
| `async-trait` boxing overhead at cold-path invocations | Low — per-invocation allocation in already-async context | Acceptable trade-off for stable ergonomics; revisit when RPITIT Send bound stabilises fully |

---

---

## 13. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Serverless Runtime PRD**: [modules/serverless-runtime/docs/PRD.md](../../serverless-runtime/docs/PRD.md)
- **Serverless Runtime DESIGN**: [modules/serverless-runtime/docs/DESIGN.md](../../serverless-runtime/docs/DESIGN.md)
