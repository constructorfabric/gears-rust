<!--
Created: 2026-05-19 by Constructor Tech
Updated: 2026-05-19 by Constructor Tech
-->

# Decomposition: CF/Gears Serverless Runtime SDK


<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Domain Foundation & Error Model - HIGH](#21-domain-foundation--error-model---high)
  - [2.2 Handler Traits - HIGH](#22-handler-traits---high)
  - [2.3 Invocation Context & Environment - HIGH](#23-invocation-context--environment---high)
  - [2.4 Trace Instrumentation - MEDIUM](#24-trace-instrumentation---medium)
  - [2.5 RuntimeAdapter Trait Surface - HIGH](#25-runtimeadapter-trait-surface---high)
  - [2.6 Host Index Client - HIGH](#26-host-index-client---high)
  - [2.7 Adapter Conformance Suite - MEDIUM](#27-adapter-conformance-suite---medium)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-status-overall`
## 1. Overview

The SDK is a Rust contract crate (`serverless-runtime-sdk`) consumed by the
`serverless-runtime` host crate and by every runtime plugin. The DESIGN identifies
eight components (`error`, `handler`, `workflow`, `context`, `environment`, `trace`,
`adapter`, `runtime-client`), two interaction sequences (handler call, compensation),
and four trait contracts (`FunctionHandler`, `WorkflowHandler`, `RuntimeAdapter`,
`ServerlessRuntimeClient`). The decomposition groups these into seven mutually
exclusive features ordered by dependency.

The strategy reflects three forces:

- The **shared domain layer** (value types + error model) must land first because every
  other feature depends on it.
- The **handler-author surface** (traits, context, environment) is split from the
  **host↔plugin boundary surface** (`RuntimeAdapter`, `ServerlessRuntimeClient`) so the
  two audiences can be reviewed and implemented independently.
- The **adapter conformance suite** ships last because it exercises everything above.

The crate is a library — there is no REST endpoint, no database, and no runtime
service. "API" and "Data" sections from the kit template are therefore absent in each
feature entry below.

## 2. Entries

### 2.1 [Domain Foundation & Error Model](features/0001-cpt-cf-serverless-runtime-plugin-sdk-feature-domain-foundation.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-feature-domain-foundation`

- **Purpose**: Stand up the crate skeleton (Cargo workspace integration, dependency
  lockdown, lint policy, doc policy) and ship the shared domain value types plus the
  `ServerlessSdkError` enum. This feature establishes the foundation every other
  feature depends on.

- **Depends On**: None

- **Scope**:
  - Cargo crate skeleton `serverless-runtime-sdk` registered in the workspace.
  - Dependency lockdown to `serde`, `serde_json`, `thiserror`, `async-trait`, `tracing`,
    `cf-credstore-sdk` — enforced by `cargo deny` config.
  - Workspace lint `unsafe_code = "forbid"`.
  - Crate-level `#![deny(missing_docs)]`.
  - Shared value types: `InvocationRecord`, `CompensationContext`, `RuntimeErrorCategory`,
    `RuntimeErrorPayload`, `RetryPolicy`, `TimelineEventType` — each `#[non_exhaustive]`
    with a documented public construction path (`::new(...)`, builder, `Default`, `From`,
    or `TryFrom` — see ADR-0003 for the criterion).
  - `error.rs` — `ServerlessSdkError` `#[non_exhaustive]` enum with `thiserror` and a
    documented mapping to `RuntimeErrorCategory`.

- **Out of scope**:
  - Handler / workflow / context / environment surface (Feature 2.2 and 2.3).
  - Adapter and client traits (Features 2.5 and 2.6).
  - Trace instrumentation (Feature 2.4).
  - Conformance suite (Feature 2.7).

- **Requirements Covered**:
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-error-model`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-nfr-no-engine-deps`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-nfr-no-unsafe`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-nfr-api-docs`

- **Design Principles Covered**:
  - `cpt-cf-serverless-runtime-plugin-sdk-principle-impl-agnostic`
  - `cpt-cf-serverless-runtime-plugin-sdk-principle-minimal-surface`

- **Design Constraints Covered**:
  - `cpt-cf-serverless-runtime-plugin-sdk-constraint-no-engine-deps`
  - `cpt-cf-serverless-runtime-plugin-sdk-constraint-stable-rust`
  - `cpt-cf-serverless-runtime-plugin-sdk-constraint-trust-boundary`

- **Domain Model Entities**:
  - `InvocationRecord`, `CompensationContext`, `RuntimeErrorCategory`, `RuntimeErrorPayload`,
    `RetryPolicy`, `TimelineEventType`, `ServerlessSdkError`.

- **Design Components**:
  - `cpt-cf-serverless-runtime-plugin-sdk-component-error`

- **Sequences**:
  - None — value types and error model are passive.

---

### 2.2 [Handler Traits](features/0002-cpt-cf-serverless-runtime-plugin-sdk-feature-handler-traits.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-feature-handler-traits`

- **Purpose**: Define the canonical handler-authoring shape that every adapter wraps
  around its backend's authoring asset — `FunctionHandler<I, O>` for stateless functions
  and `WorkflowHandler<I, O>: FunctionHandler<I, O>` for durable workflows with
  compensation, together with the `CompensationInput` value passed to the rollback path.

- **Depends On**: `cpt-cf-serverless-runtime-plugin-sdk-feature-domain-foundation`

- **Scope**:
  - `handler.rs` — `FunctionHandler<I, O>` async trait (`#[async_trait]`) with
    `Send + Sync + 'static` bound on `Self`.
  - `workflow.rs` — `WorkflowHandler<I, O>: FunctionHandler<I, O>` supertrait with
    `compensate(...)` method.
  - `CompensationInput` `#[non_exhaustive]` struct with `::new(...)` constructor.
  - `CompensationTrigger` `#[non_exhaustive]` enum.
  - Trait-level rustdoc covering implementation expectations and concurrency invariants.

- **Out of scope**:
  - Context construction and Environment resolution (Feature 2.3).
  - Trace instrumentation around handler calls (Feature 2.4).
  - Dispatch through `RuntimeAdapter::execute` (Feature 2.5).

- **Requirements Covered**:
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-handler-trait`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-handler-send-sync`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-workflow-handler-trait`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-compensation-input`

- **Design Principles Covered**:
  - `cpt-cf-serverless-runtime-plugin-sdk-principle-impl-agnostic`

- **Design Constraints Covered**:
  - Inherits from Feature 2.1.

- **Domain Model Entities**:
  - `FunctionHandler<I, O>`, `WorkflowHandler<I, O>`, `CompensationInput`, `CompensationTrigger`.

- **Design Components**:
  - `cpt-cf-serverless-runtime-plugin-sdk-component-handler`
  - `cpt-cf-serverless-runtime-plugin-sdk-component-workflow`
  - `cpt-cf-serverless-runtime-plugin-sdk-interface-handler-trait`
  - `cpt-cf-serverless-runtime-plugin-sdk-interface-workflow-trait`

- **Sequences**:
  - None — the call sequences are emitted from Feature 2.4 (trace).

---

### 2.3 [Invocation Context & Environment](features/0003-cpt-cf-serverless-runtime-plugin-sdk-feature-context-environment.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-feature-context-environment`

- **Purpose**: Ship the handler-facing projection of `InvocationRecord` (`Context`)
  and the synchronous, pre-fetched config/secret access surface (`Environment` +
  `CredStoreEnvironment`). These are the values handlers consume on every call.

- **Depends On**: `cpt-cf-serverless-runtime-plugin-sdk-feature-domain-foundation`

- **Scope**:
  - `context.rs` — `Context` struct projected from `InvocationRecord` with the nine
    documented fields (`invocation_id`, `function_id`, `function_version`, `tenant_id`,
    `attempt_number`, `correlation_id`, `trace_id`, `span_id`, deadline) and a
    `::from_invocation_record(...)` constructor.
  - `is_deadline_exceeded()` and `remaining_time()` helpers on `Context`.
  - `environment.rs` — synchronous `Environment` trait with `get_config` / `get_secret`
    returning `Option<&str>` borrowed from `&self`.
  - `CredStoreEnvironment` implementation backed by an owned `HashMap<String, String>`
    populated at construction time from `cf-credstore-sdk`.

- **Out of scope**:
  - Adapter-side population of `Context` and `Environment` per invocation
    (Feature 2.5 — happens inside `RuntimeAdapter::execute`).
  - Trace span attribute emission from `Context` (Feature 2.4).

- **Requirements Covered**:
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-context`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-deadline-helpers`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-environment-trait`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-nfr-authoring-ergonomics`

- **Design Principles Covered**:
  - `cpt-cf-serverless-runtime-plugin-sdk-principle-gts-by-reference`

- **Design Constraints Covered**:
  - Inherits from Feature 2.1.

- **Domain Model Entities**:
  - `Context`, `Environment`, `CredStoreEnvironment`.

- **Design Components**:
  - `cpt-cf-serverless-runtime-plugin-sdk-component-context`
  - `cpt-cf-serverless-runtime-plugin-sdk-component-environment`

- **Sequences**:
  - None — Context/Environment are values consumed in sequences owned by Feature 2.4.

---

### 2.4 [Trace Instrumentation](features/0004-cpt-cf-serverless-runtime-plugin-sdk-feature-trace-instrumentation.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-feature-trace-instrumentation`

- **Purpose**: Provide the adapter-only `trace` helpers (`call_instrumented` and
  `compensate_instrumented`) that emit uniform `TimelineEventType` events around handler
  invocations. Owns both interaction sequences from DESIGN §3.6.

- **Depends On**: `cpt-cf-serverless-runtime-plugin-sdk-feature-handler-traits`, `cpt-cf-serverless-runtime-plugin-sdk-feature-context-environment`

- **Scope**:
  - `trace.rs` — `call_instrumented<H, I, O>` and `compensate_instrumented<H, I, O>` free
    functions generic over the handler signatures.
  - `tracing::info_span!` creation with the documented field set (invocation_id,
    function_id, tenant_id, attempt_number, correlation_id, optional trace/span ids,
    compensation-only fields).
  - Lifecycle event emission: `started`, `succeeded`, `failed`, and `compensation_*`
    variants mapped to `TimelineEventType`.
  - Documentation marking the module as adapter-only by convention.

- **Out of scope**:
  - `tracing` subscriber setup (adapter / platform concern).
  - OpenTelemetry exporter wiring (adapter / platform concern).
  - Structured log routing and metrics (out of SDK scope).

- **Requirements Covered**:
  - [ ] `p2` - `cpt-cf-serverless-runtime-plugin-sdk-fr-trace-module`
  - [ ] `p2` - `cpt-cf-serverless-runtime-plugin-sdk-fr-no-consumer-tracing`
  - [ ] `p2` - `cpt-cf-serverless-runtime-plugin-sdk-nfr-low-overhead`

- **Design Principles Covered**:
  - Inherits from Features 2.1 and 2.2.

- **Design Constraints Covered**:
  - Inherits from Feature 2.1.

- **Domain Model Entities**:
  - `TimelineEventType` (consumed; defined in Feature 2.1).

- **Design Components**:
  - `cpt-cf-serverless-runtime-plugin-sdk-component-trace`

- **Sequences**:
  - `cpt-cf-serverless-runtime-plugin-sdk-seq-handler-call`
  - `cpt-cf-serverless-runtime-plugin-sdk-seq-compensate`

---

### 2.5 [RuntimeAdapter Trait Surface](features/0005-cpt-cf-serverless-runtime-plugin-sdk-feature-runtime-adapter.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-feature-runtime-adapter`

- **Purpose**: Declare the primary host↔plugin contract — the `RuntimeAdapter` async
  trait — together with the `Schedule`, `Trigger`, and `BindingHandle` value types the
  trait carries across the boundary. Owns invocation, control, schedule, and
  event-trigger method classes per parent ADR `cpt-cf-serverless-runtime-adr-thin-host`.

- **Depends On**: `cpt-cf-serverless-runtime-plugin-sdk-feature-domain-foundation`, `cpt-cf-serverless-runtime-plugin-sdk-feature-handler-traits`, `cpt-cf-serverless-runtime-plugin-sdk-feature-context-environment`

- **Scope**:
  - `adapter.rs` — `RuntimeAdapter` async trait (`#[async_trait]`,
    `Send + Sync + 'static`).
  - Invocation method accepting `InvocationRecord` and driving the adapter's internal
    handler bridge.
  - Control methods covering cancel, suspend, and resume by `invocation_id`.
  - Schedule binding methods (bind / update / revoke) accepting `Schedule` values.
  - Event-trigger binding methods (bind / update / revoke) accepting `Trigger` values.
  - `Schedule`, `Trigger`, and `BindingHandle` `#[non_exhaustive]` value types with
    constructors; no engine-specific fields.

- **Out of scope**:
  - Backend-native schedule firing and event matching (lives inside each plugin).
  - REST/CRUD persistence for schedule and trigger definitions (host control plane).
  - Host index event emission (Feature 2.6).
  - `JobTransport` / `ExecutionContext` callback surface — explicitly removed by parent
    ADR `cpt-cf-serverless-runtime-adr-thin-host`.

- **Requirements Covered**:
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-adapter-invoke`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-adapter-control`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-adapter-schedule`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-adapter-event-trigger`
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-schedule-trigger-types`

- **Design Principles Covered**:
  - Inherits from Feature 2.1.

- **Design Constraints Covered**:
  - Inherits from Feature 2.1.

- **Domain Model Entities**:
  - `RuntimeAdapter`, `Schedule`, `Trigger`, `BindingHandle`.

- **Design Components**:
  - `cpt-cf-serverless-runtime-plugin-sdk-component-adapter`
  - `cpt-cf-serverless-runtime-plugin-sdk-interface-runtime-adapter`

- **Sequences**:
  - None new — the invocation sequence is emitted from Feature 2.4 (trace).

---

### 2.6 [Host Index Client](features/0006-cpt-cf-serverless-runtime-plugin-sdk-feature-runtime-client.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-feature-runtime-client`

- **Purpose**: Declare the plugin→host event port (`ServerlessRuntimeClient`) and the
  `InvocationIndexEvent` value type that feeds the host's lightweight invocation index.
  This is the load-bearing mechanism behind parent ADR
  `cpt-cf-serverless-runtime-adr-thin-host`'s host-indexed / plugin-detailed split.

- **Depends On**: `cpt-cf-serverless-runtime-plugin-sdk-feature-domain-foundation`

- **Scope**:
  - `client.rs` — `ServerlessRuntimeClient` async trait (`#[async_trait]`,
    `Send + Sync + 'static`).
  - `InvocationIndexEvent` `#[non_exhaustive]` struct carrying the indexed fields per
    parent ADR `cpt-cf-serverless-runtime-adr-thin-host`: `invocation_id`, `function_id`, `adapter`, `tenant`, `owner`, `status`,
    `timestamps`, `error_summary`.
  - `InvocationIndexEvent::new(...)` constructor + `.with_*` setters for forward-compatible
    population across SDK minor versions.

- **Out of scope**:
  - Host-side index storage and REST aggregate-query implementation (host concern).
  - Deep-fetch path for full timeline / payloads (host concern; delegated back to plugins).
  - Any general-purpose callback surface (`JobTransport`, `ExecutionContext`, `checkpoint`,
    `wait_for_event`, `sleep`) — explicitly removed by parent ADR
    `cpt-cf-serverless-runtime-adr-thin-host`.

- **Requirements Covered**:
  - [ ] `p1` - `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-client-index-events`

- **Design Principles Covered**:
  - Inherits from Feature 2.1.

- **Design Constraints Covered**:
  - Inherits from Feature 2.1.

- **Domain Model Entities**:
  - `ServerlessRuntimeClient`, `InvocationIndexEvent`.

- **Design Components**:
  - `cpt-cf-serverless-runtime-plugin-sdk-component-runtime-client`
  - `cpt-cf-serverless-runtime-plugin-sdk-interface-runtime-client`

- **Sequences**:
  - None — emission is a single async call.

---

### 2.7 [Adapter Conformance Suite](features/0007-cpt-cf-serverless-runtime-plugin-sdk-feature-conformance-suite.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-feature-conformance-suite`

- **Purpose**: Ship a reusable conformance test suite that every plugin runs against
  its `RuntimeAdapter` implementation. The suite is the load-bearing mechanism for
  uniform user-visible semantics across backends, per parent ADR
  `cpt-cf-serverless-runtime-adr-thin-host`.

- **Depends On**: `cpt-cf-serverless-runtime-plugin-sdk-feature-domain-foundation`, `cpt-cf-serverless-runtime-plugin-sdk-feature-handler-traits`, `cpt-cf-serverless-runtime-plugin-sdk-feature-context-environment`, `cpt-cf-serverless-runtime-plugin-sdk-feature-trace-instrumentation`, `cpt-cf-serverless-runtime-plugin-sdk-feature-runtime-adapter`, `cpt-cf-serverless-runtime-plugin-sdk-feature-runtime-client`

- **Scope**:
  - Conformance test fixtures keyed off the `FunctionHandler` signature.
  - Coverage areas: invocation status transitions, retry semantics, compensation
    triggering, suspension / resume visibility, and error taxonomy.
  - Reusable helpers (mock `ServerlessRuntimeClient`, deterministic `Environment`,
    canned `InvocationRecord` factories) that adapter authors plug their `RuntimeAdapter`
    impl into.
  - Documentation describing how to run the suite from a plugin crate.

- **Out of scope**:
  - Adapter-specific fixtures (each plugin author wires its backend setup).
  - Performance benchmarks (separate concern; not part of conformance).
  - End-to-end tests against real backends (lives in each plugin crate).

- **Requirements Covered**:
  - [ ] `p2` - `cpt-cf-serverless-runtime-plugin-sdk-fr-conformance-suite`

- **Design Principles Covered**:
  - Inherits from Features 2.1–2.6.

- **Design Constraints Covered**:
  - Inherits from Feature 2.1.

- **Domain Model Entities**:
  - None new — the suite exercises every type from Features 2.1–2.6.

- **Design Components**:
  - None new — the suite is a test harness that exercises every component above.

- **Sequences**:
  - None new — the suite asserts the sequences owned by Feature 2.4.

---

## 3. Feature Dependencies

```text
cpt-cf-serverless-runtime-plugin-sdk-feature-domain-foundation
    │
    ├─→ cpt-cf-serverless-runtime-plugin-sdk-feature-handler-traits
    │       │
    │       ├─→ cpt-cf-serverless-runtime-plugin-sdk-feature-trace-instrumentation
    │       │       │
    │       │       └─→ cpt-cf-serverless-runtime-plugin-sdk-feature-conformance-suite
    │       │
    │       └─→ cpt-cf-serverless-runtime-plugin-sdk-feature-runtime-adapter
    │               │
    │               └─→ cpt-cf-serverless-runtime-plugin-sdk-feature-conformance-suite
    │
    ├─→ cpt-cf-serverless-runtime-plugin-sdk-feature-context-environment
    │       │
    │       ├─→ cpt-cf-serverless-runtime-plugin-sdk-feature-trace-instrumentation
    │       └─→ cpt-cf-serverless-runtime-plugin-sdk-feature-runtime-adapter
    │
    └─→ cpt-cf-serverless-runtime-plugin-sdk-feature-runtime-client
            │
            └─→ cpt-cf-serverless-runtime-plugin-sdk-feature-conformance-suite
```

**Dependency rationale**:

- `feature-handler-traits` requires `feature-domain-foundation`: handler traits reference
  `ServerlessSdkError` and the shared value types defined in the foundation.
- `feature-context-environment` requires `feature-domain-foundation`: `Context` is a
  projection of `InvocationRecord`; `Environment` returns values whose error type is
  `ServerlessSdkError`.
- `feature-trace-instrumentation` requires `feature-handler-traits` and
  `feature-context-environment`: `call_instrumented` / `compensate_instrumented` are
  generic over the handler signature and emit span fields drawn from `Context`.
- `feature-runtime-adapter` requires `feature-domain-foundation`,
  `feature-handler-traits`, and `feature-context-environment`: the adapter's invocation
  method dispatches handlers and constructs `Context` / `Environment` per call.
- `feature-runtime-client` requires `feature-domain-foundation` only: it carries
  `InvocationIndexEvent` fields drawn from the shared domain.
- `feature-conformance-suite` requires every preceding feature: it asserts the
  invocation, control, schedule, and event-trigger contracts on `RuntimeAdapter`, the
  index event contract on `ServerlessRuntimeClient`, and the trace / error taxonomy
  emitted by the trace module.
- `feature-handler-traits` and `feature-context-environment` are independent of each
  other and can be developed in parallel after the foundation lands.
- `feature-runtime-client` is independent of every feature except the foundation and can
  be developed in parallel with the handler-author cluster.
