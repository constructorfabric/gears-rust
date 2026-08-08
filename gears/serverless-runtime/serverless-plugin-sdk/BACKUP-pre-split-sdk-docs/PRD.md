<!--
Created: 2026-03-30 by Constructor Tech
Updated: 2026-03-30 by Constructor Tech
-->

# PRD — CF/Gears Serverless Runtime SDK


<!-- toc -->

- [PRD — CF/Gears Serverless Runtime SDK](#prd--cfgears-serverless-runtime-sdk)
  - [1. Overview](#1-overview)
    - [1.1 Purpose](#11-purpose)
    - [1.2 Background / Problem Statement](#12-background--problem-statement)
    - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
    - [1.4 Glossary](#14-glossary)
  - [2. Actors](#2-actors)
    - [2.1 Human Actors](#21-human-actors)
      - [Function Author](#function-author)
      - [Adapter Developer](#adapter-developer)
    - [2.2 System Actors](#22-system-actors)
      - [Serverless Runtime Host](#serverless-runtime-host)
      - [Runtime Adapter Crate](#runtime-adapter-crate)
  - [3. Operational Concept \& Environment](#3-operational-concept--environment)
    - [3.1 Gear-Specific Environment Constraints](#31-gear-specific-environment-constraints)
  - [4. Scope](#4-scope)
    - [4.1 In Scope](#41-in-scope)
    - [4.2 Out of Scope](#42-out-of-scope)
  - [5. Functional Requirements](#5-functional-requirements)
    - [5.1 FunctionHandler Trait](#51-functionhandler-trait)
      - [Async Typed FunctionHandler](#async-typed-functionhandler)
      - [FunctionHandler Concurrent-Use Guarantee](#functionhandler-concurrent-use-guarantee)
    - [5.2 WorkflowHandler Trait](#52-workflowhandler-trait)
      - [Workflow Compensation](#workflow-compensation)
      - [CompensationInput](#compensationinput)
    - [5.3 Invocation Context](#53-invocation-context)
      - [Context Populated from InvocationRecord](#context-populated-from-invocationrecord)
      - [Deadline Helpers](#deadline-helpers)
    - [5.4 Environment](#54-environment)
      - [Config and Secret Access](#config-and-secret-access)
    - [5.5 Error Model](#55-error-model)
      - [ServerlessSdkError Variants](#serverlesssdkerror-variants)
    - [5.6 Tracing Instrumentation](#56-tracing-instrumentation)
      - [Adapter-Facing Timeline Instrumentation](#adapter-facing-timeline-instrumentation)
      - [No Consumer-Visible Tracing](#no-consumer-visible-tracing)
    - [5.7 Adapter Conformance Suite](#57-adapter-conformance-suite)
      - [Conformance Test Suite](#conformance-test-suite)
    - [5.8 RuntimeAdapter Trait](#58-runtimeadapter-trait)
      - [Invocation Method](#invocation-method)
      - [Control Methods](#control-methods)
      - [Schedule Method](#schedule-method)
      - [Event-Trigger Method](#event-trigger-method)
      - [Schedule and Trigger Value Types](#schedule-and-trigger-value-types)
    - [5.9 ServerlessRuntimeClient Trait](#59-serverlessruntimeclient-trait)
      - [Invocation-Index Event Emission](#invocation-index-event-emission)
  - [6. Non-Functional Requirements](#6-non-functional-requirements)
    - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
      - [No Engine-Specific Dependencies](#no-engine-specific-dependencies)
      - [Zero Unsafe Code](#zero-unsafe-code)
      - [Low Per-Invocation Overhead](#low-per-invocation-overhead)
      - [Public API Documentation](#public-api-documentation)
      - [Authoring Ergonomics](#authoring-ergonomics)
    - [6.2 NFR Exclusions](#62-nfr-exclusions)
  - [7. Public Library Interfaces](#7-public-library-interfaces)
    - [7.1 Public API Surface](#71-public-api-surface)
    - [7.2 External Integration Contracts](#72-external-integration-contracts)
  - [8. Use Cases](#8-use-cases)
    - [Adapter Developer Implements a FunctionHandler](#adapter-developer-implements-a-functionhandler)
    - [Adapter Developer Wires SDK into an Adapter Crate](#adapter-developer-wires-sdk-into-an-adapter-crate)
    - [Adapter Developer Implements Workflow Compensation](#adapter-developer-implements-workflow-compensation)
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
  ✓ Actors (users, systems) that interact with this gear
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

`serverless-runtime-sdk` is the single engine-agnostic contract crate of the
CF/Gears Serverless Runtime gear, written against stable Rust. It defines the
types and traits shared by the main gear and by runtime plugins.

The invocation flow:

```mermaid
flowchart LR
    consumer([other gear])
    main([main gear])
    plugin([runtime plugin])
    fn([authored function])

    consumer -- "invoke via ServerlessRuntimeClientV1" --> main
    main -- "dispatch via RuntimeAdapter" --> plugin
    plugin -- "runs via FunctionHandler / WorkflowHandler" --> fn
    plugin -. "index events via InvocationIndexPort" .-> main
```

Every arrow label is a trait declared in this crate — the SDK is the edges of this
diagram, not a node. The `FunctionHandler` impl itself is plugin-owned Rust; the
authored function is what the Function Author wrote (a Starlark script, a WASM module,
a deployed Lambda function, a workflow definition).

**Traits — who implements, who calls:**

| Trait | Implemented by | Called by |
|---|---|---|
| `ServerlessRuntimeClientV1` | main gear | other gears |
| `RuntimeAdapter` | runtime plugin | main gear |
| `InvocationIndexPort` | main gear | runtime plugin (to emit index-update events) |
| `FunctionHandler<I, O>` / `WorkflowHandler<I, O>` | runtime plugin (wraps the function author's authored function) | the same plugin, from inside its invocation bridge |

> **Note on "who implements `FunctionHandler`".** The `impl FunctionHandler<I, O> for …` lives inside the runtime plugin: the plugin provides a Rust type that wraps the function author's authored function (a Starlark script, a Rust activity fn, a compiled WASM module, a deployed Lambda function, …) and executes it from inside `call`. Function authors do not implement SDK traits directly; they author in the plugin's own authoring model and the plugin bridges that into a `FunctionHandler`. A plugin could expose `FunctionHandler` directly as its authoring model for power users, but that's a minority case. This is why the handler traits are both implemented and called by the plugin — the plugin is on both sides of that boundary.

**Shared domain** (used across all parties): `InvocationRecord`,
`CompensationContext`, `RuntimeErrorCategory`, `RuntimeErrorPayload`, `RetryPolicy`,
`TimelineEventType`, and the `Schedule` / `Trigger` value types that cross the
`RuntimeAdapter` boundary (§4.1).

**Handler-author projections** (ergonomic views of the shared domain): `Context` (from
`InvocationRecord`), `CompensationInput` (from `CompensationContext`), `ServerlessSdkError`
(maps to `RuntimeErrorCategory`).

**Other modules**: `environment` (the `Environment` trait with a standard
`CredStoreEnvironment` implementation for synchronous config/secret access), `trace`
(plugin-only helper that emits `TimelineEventType` events around handler calls).

Invocation plumbing (`Context` construction from `InvocationRecord`, `Environment`
population from the credstore, typed handler-input deserialisation) is performed by each
runtime plugin using its backend-native primitives. This SDK does not centralise a
`dispatch` helper; uniform user-visible semantics are enforced by the typed contracts
in this crate (trait shapes, error mapping, timeline event taxonomy) together with the
plugin conformance suite shipped by this crate (§5.7), which every plugin runs against
its `RuntimeAdapter` implementation.

Workflow checkpointing, suspend/resume, and step-level compensation are plugin-specific
concerns — each plugin uses its backend's native compensation primitives (Temporal saga
patterns, SQS DLQ + compensation queue, Azure Durable compensation activities, etc.) and
these are out of scope for this crate. Plugins write checkpoints; the SDK never reads
them — it only carries the state a plugin chose to expose to a compensation handler, as
the opaque `CompensationInput.workflow_state_snapshot` payload.

### 1.2 Background / Problem Statement

The CF/Gears Serverless Runtime
(`cpt-cf-serverless-runtime-principle-impl-agnostic`) is designed to support
multiple execution engines through a pluggable adapter model. Without a shared,
engine-agnostic SDK core, each adapter would define its own handler contract,
preventing the platform from enforcing a consistent authoring contract, error
model, or observability surface across engines.

A stable, engine-agnostic SDK core solves this by defining the contract once.
Adapter developers implement it; the runtime depends on its types. This enables
adapter portability, consistent error classification, uniform observability, and
a single authoring mental model regardless of the underlying engine technology.

### 1.3 Goals (Business Outcomes)

_Baseline: gear is new (no prior implementation). All targets apply at first stable release (v0.1.0)._

- **FunctionHandler portability**: Adapter developers can implement handlers against a single
  contract that works unchanged across any CF/Gears execution engine.
  _Target: zero engine-specific changes required to port a conformant adapter between engines, verified by the adapter conformance test suite (`cpt-cf-serverless-runtime-plugin-sdk-fr-conformance-suite`, §5.7)._
- **Error categorisation completeness**: The SDK error model maps unambiguously to runtime
  `RuntimeErrorCategory` values, enabling correct retry and dead-letter routing without
  adapter-specific error handling.
  _Target: 100% of `ServerlessSdkError` variants carry a documented `RuntimeErrorCategory` mapping; no unmapped (`Unknown`) fallback case._
- **Observability zero-overhead for consumers**: Adapters can instrument every invocation
  with structured tracing spans and timeline events without requiring SDK consumers to add
  any observability code.
  _Target: a conformant `FunctionHandler` implementation contains zero `tracing` imports, verified by compile-time import audit in CI._
- **Toolchain stability**: The crate compiles without unsafe code and with zero engine-specific transitive dependencies, both enforced on every CI run.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| **FunctionHandler** | A Rust type that implements the `FunctionHandler<I, O>` trait to service function invocations. |
| **WorkflowHandler** | A `FunctionHandler` that additionally implements compensation for durable workflow rollback. |
| **Context** | Read-only invocation metadata (ID, tenant, attempt, deadline) derived from `InvocationRecord`. |
| **Environment** | Read-only abstraction over configuration and secret access for a handler invocation. |
| **Compensation** | The rollback contract for durable workflows in CF/Gears. Two layers: function-level compensation is implemented by the adapter author via `WorkflowHandler::compensate` and invoked by the platform as a standard invocation with a `CompensationInput` payload; step-level compensation (sub-step rollback within a workflow execution) is owned by the runtime adapter using its backend's native compensation primitives, not the SDK. |
| **Runtime Adapter** | A CF/Gears runtime adapter crate that implements the `RuntimeAdapter` trait from this SDK and drives handlers. Each adapter uses its backend's native primitives (Temporal signals/schedules, cloud-FaaS workflows, etc.) for invocation, scheduling, and event-triggered dispatch. |
| **GTS ID** | An opaque Global Type System identifier string; the SDK carries these as `String` without interpretation. |
| **Timeline Event** | A structured tracing event mapping to `InvocationTimelineEvent` in the runtime domain. |

---

## 2. Actors

### 2.1 Human Actors

#### Function Author

**ID**: `cpt-cf-serverless-runtime-plugin-sdk-actor-fn-author`

- **Role**: A developer who writes functions or workflows on CF/Gears using an
  adapter-provided authoring model (e.g., Starlark scripts, Temporal activities). Does
  not implement SDK traits directly — the adapter mediates between the authoring model
  and `FunctionHandler<I, O>` / `WorkflowHandler<I, O>`. An indirect stakeholder: their
  needs inform what adapters must expose, which in turn shapes SDK contracts.
- **Needs**: A predictable invocation contract (typed I/O, context, error semantics)
  that adapter authors can surface in the adapter's authoring model.

#### Adapter Developer

**ID**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

- **Role**: A platform developer building an adapter crate (e.g., Starlark, Temporal)
  within the Serverless Runtime. Implements the SDK handler contracts
  (`FunctionHandler`, `WorkflowHandler`), wires the `trace` module for invocation
  instrumentation, and integrates SDK-defined types (`Context`, `ServerlessSdkError`)
  into the runtime's invocation-routing and error-categorisation logic.
- **Needs**: Ergonomic, well-documented handler contracts they can implement from
  the SDK documentation alone; instrumentation utilities that emit consistent
  timeline events without manual wiring; unambiguous contracts for how `Context`
  and `Environment` are populated from engine data; SDK-defined types that remain
  structurally stable across minor versions; a consistent mapping from
  `ServerlessSdkError` variants to `RuntimeErrorCategory` for retry and dead-letter
  routing decisions.

### 2.2 System Actors

#### Serverless Runtime Host

**ID**: `cpt-cf-serverless-runtime-plugin-sdk-actor-runtime-host`

- **Role**: The host process inside the Serverless Runtime gear that loads runtime
  adapter crates, drives invocations through `RuntimeAdapter`, and exposes the
  `ServerlessRuntimeClient` back to adapters for emitting lightweight invocation-index
  events. Owns the dispatch loop, deadline tracking, and the population of
  `InvocationRecord` fields that the SDK surfaces to handlers via `Context`.

#### Runtime Adapter Crate

**ID**: `cpt-cf-serverless-runtime-plugin-sdk-actor-runtime-adapter`

- **Role**: An engine-specific crate (e.g., Starlark, Temporal) that depends on the SDK
  to implement `RuntimeAdapter`, `FunctionHandler<I, O>`, and `WorkflowHandler<I, O>`.
  Wraps an authored function produced by the Function Author, translates engine-native
  data into SDK-defined types (`Context`, `Environment`, `ServerlessSdkError`), and
  emits adapter-facing timeline events through the SDK `trace` module.

---

## 3. Operational Concept & Environment

### 3.1 Gear-Specific Environment Constraints

- It must contain **no unsafe code** (enforced by workspace `unsafe_code = "forbid"`).
- It must have **no engine-specific dependencies** (no `temporal-sdk`, `starlark`,
  or similar crates) — ever.
- **Developer experience target** (UX-PRD-001): The SDK MUST be designed so that
  an Adapter Developer — a platform developer with intermediate async Rust experience —
  can implement a conformant `FunctionHandler` using only the SDK documentation and
  this PRD, without consulting engine documentation. This is a design quality objective
  for the SDK, not a prerequisite on the developer.
- **Compatibility policy**: The crate follows Rust semver conventions. At 0.x all
  public interfaces are considered unstable and may change between minor releases.
  The stability target is 1.0, gated on successful validation by at least one
  production adapter.

---

## 4. Scope

### 4.1 In Scope

**Shared domain types** — `InvocationRecord`, `CompensationContext`,
`RuntimeErrorCategory`, `RuntimeErrorPayload`, `RetryPolicy`, `TimelineEventType`,
plus the schedule/event-trigger value types (`Schedule`, `Trigger`)
that cross the `RuntimeAdapter` boundary. The host hands schedule and trigger
definitions to the adapter through `RuntimeAdapter::schedule` and
`RuntimeAdapter::event_trigger`; the adapter wires them to its backend's native
primitive. The SDK owns the in-process Rust types; lifecycle persistence and the
REST control plane for those definitions are host concerns, not SDK concerns
(see §4.2).

**Traits**:

- `RuntimeAdapter` — *implemented by each runtime adapter, called by the host*.
  Methods for invocation, control (cancel/suspend/resume), schedule, and event-trigger.
- `ServerlessRuntimeClient` — *implemented by the host, called by adapters* to emit
  events for the lightweight invocation index (invocation_id, function_id, adapter,
  tenant, owner, status, timestamps, error summary).
- `FunctionHandler<I, O>` — *implemented by the runtime adapter (wrapping the function
  author's authored function), called by the adapter*. Contract for stateless function
  invocations.
- `WorkflowHandler<I, O>` — *implemented by the runtime adapter (wrapping the function
  author's authored function), called by the adapter*. Extends `FunctionHandler` with
  compensation.
- `Environment` — *implemented by the adapter (via `CredStoreEnvironment` or a custom
  impl), called from inside the `FunctionHandler::call` execution* for synchronous
  config and secret access.

**Handler-author projections**:

- `Context` (projection of `InvocationRecord` passed to handlers).
- `CompensationInput` and `CompensationTrigger` (projection of `CompensationContext`
  for saga compensation).
- `ServerlessSdkError` typed error enum with documented `RuntimeErrorCategory` mapping.

**Other modules**:

- `trace` module — adapter-only instrumentation utilities that emit
  `tracing` spans and `TimelineEventType` events.

### 4.2 Out of Scope

- GTS schema validation or GTS chain parsing — schema ownership belongs to the GTS layer.
- `InvocationStatus` state machine — owned by the runtime (host).
- `TenantRuntimePolicy` (quotas, retention, runtime allowlist) — host-only governance
  concern, not an SDK type. Adapters never see `TenantRuntimePolicy`.
- Lifecycle, persistence, REST CRUD, and tenant-level governance of schedule and event
  trigger **definitions** — host control-plane concerns. The SDK exports only the
  in-process Rust types (`Schedule`, `Trigger`) that the host passes to plugins through
  `RuntimeAdapter::schedule` and `RuntimeAdapter::event_trigger` (§5.8). Schedule
  firing and event-trigger matching at runtime are performed inside each adapter using
  its backend's native primitives (Temporal Schedule API and signals, EventBridge
  Scheduler and SQS for Lambda, Azure Durable timers, etc.). The SDK ships no
  scheduler, timer wheel, or event-matching engine.
- Retry *execution* — the `RetryPolicy` *type* (max attempts, backoff, non-retryable
  error classification) is in scope and exported from this SDK's `domain` module so every
  adapter honours it consistently, but actually *applying* the policy — scheduling retries,
  dead-lettering after exhaustion — is performed by each adapter using its backend-native
  retry mechanism (Temporal retry policy, SQS retry + DLQ, Step Functions catcher, etc.).
  The SDK also exposes `attempt_number` in `Context` so handlers can be retry-aware.
- Durability primitives for in-process runtimes (Starlark, WASM) — in-process adapters
  that lack native durable timers, event signals, or checkpointing will consume a shared
  adapter-level helper crate (out of scope for this SDK). Keeping those primitives outside
  the SDK scopes the complexity to the adapters that need it instead of forcing every
  adapter to depend on a runtime-neutral substrate.
- `JobTransport` and `ExecutionContext` host-callback abstractions — explicitly excluded
  as a consequence of the parent gear's thin-host / fat-plugin architecture. Adapters
  are autonomous; the host dispatches through `dyn RuntimeAdapter` and receives index
  updates through `ServerlessRuntimeClient`. Rationale belongs in DESIGN and the parent
  ADR series, not this PRD.


---

## 5. Functional Requirements

### 5.1 FunctionHandler Trait

#### Async Typed FunctionHandler

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-handler-trait`

The crate MUST provide an async `FunctionHandler` contract parameterised over typed input `I`
and typed output `O`. It MUST expose a single invocation method that accepts typed input,
invocation context, and environment access, and returns a typed result or a `ServerlessSdkError`.

- **Rationale**: Provides the typed, adapter-neutral authoring contract for all stateless functions.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

#### FunctionHandler Concurrent-Use Guarantee

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-handler-send-sync`

`FunctionHandler` implementations MUST be safe to share across concurrent invocations dispatched
by adapters running on multi-threaded async runtimes.

- **Rationale**: Adapters run on multi-threaded async runtimes; handler instances must be safely shared.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

### 5.2 WorkflowHandler Trait

#### Workflow Compensation

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-workflow-handler-trait`

The crate MUST provide a `WorkflowHandler` contract that extends `FunctionHandler` with a
compensation method accepting invocation context, environment access, and a `CompensationInput`,
returning success or a `ServerlessSdkError`.

- **Rationale**: Implements the function-level compensation layer of the two-layer saga model
  (`cpt-cf-serverless-runtime-fr-advanced-patterns` BR-133).
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

#### CompensationInput

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-compensation-input`

The crate MUST provide a `CompensationInput` struct that carries all fields from the runtime's
`CompensationContext` (`gts.cf.core.sless.compensation_context.v1~`) required for
idempotent rollback: `trigger`, `original_workflow_invocation_id`, `failed_step_id`,
`failed_step_error` (typed: `error_type`, `message`, `error_metadata`),
`workflow_state_snapshot`, `timestamp`, `function_id`,
`original_input`, `tenant_id`, `correlation_id`, `started_at`.

Field names and optionality MUST match the runtime's `CompensationContext` schema:
`correlation_id` and `started_at` are `Option<String>` (optional in the runtime schema).

- **Rationale**: Compensation handlers need the full context to perform idempotent rollback
  without accessing the runtime domain types.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

### 5.3 Invocation Context

#### Context Populated from InvocationRecord

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-context`

The crate MUST provide a `Context` struct with the following fields:

- From `InvocationRecord`: `invocation_id`, `function_id`, `function_version`, `tenant_id`.
  `function_version` is the semantic version of the function deployment (e.g., `1.2.3`),
  not the GTS schema version embedded in `function_id`. The two are independently sourced
  from `InvocationRecord`.
- From `InvocationObservability`: `correlation_id: String` (required in
  `InvocationObservability`), `trace_id: Option<String>`, `span_id: Option<String>`.
- Adapter-supplied: `attempt_number: u32` (1-indexed; the adapter tracks retry count
  independently — the runtime's `InvocationRecord` does not expose an attempt counter,
  though retry count is tracked at the persistence layer).
- Computed: `deadline` derived from `FunctionLimits.timeout_seconds` at invocation start.

Adapter implementers validating `Context` fields should distinguish structural/type-level
violations (e.g., missing required field, wrong type) — which map to `InvalidInput` — from
business-rule rejections (e.g., tenant not allowed to invoke this function) — which map to
`UserError` (see §5.5 "InvalidInput vs UserError").

- **Rationale**: Maps the runtime's invocation record to the minimal SDK surface handlers need.
  Follows `cpt-cf-serverless-runtime-principle-impl-agnostic`: no engine-specific fields.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

#### Deadline Helpers

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-deadline-helpers`

`Context` MUST expose helpers so handlers can check whether the deadline has been exceeded
and query the remaining time before forced termination.

- **Rationale**: Enables handlers to self-terminate cleanly (returning `ServerlessSdkError::Timeout`)
  rather than being killed mid-operation, which could leave partial state.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

### 5.4 Environment

#### Config and Secret Access

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-environment-trait`

The SDK MUST provide an `Environment` trait for synchronous, **read-only**
key-based access to configuration values and secrets, populated before each
invocation. The SDK MUST
provide a standard `Environment` implementation backed by the platform credstore
(`CredStoreClientV1`). Custom `Environment` implementations remain possible for
testing or non-standard secret sources.

- **Rationale**: Provides engine-agnostic access to function configuration and credentials
  without coupling handler logic to async resolution. The credstore is the platform-standard
  secret source; centralising the integration in the SDK prevents duplicate boilerplate.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

### 5.5 Error Model

#### ServerlessSdkError Variants

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-error-model`

The crate MUST provide a `ServerlessSdkError` enum with at minimum the following variants
and their documented `RuntimeErrorCategory` mappings:

| Variant | `RuntimeErrorCategory` |
|---------|------------------------|
| `UserError` | `NonRetryable` |
| `InvalidInput` | `NonRetryable` |
| `Timeout` | `Timeout` |
| `NotSupported` | `NonRetryable` |
| `Internal` | `Retryable` |

The error type MUST be extensible — future variants MUST NOT break existing consumers.

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
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

### 5.6 Tracing Instrumentation

#### Adapter-Facing Timeline Instrumentation

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-trace-module`

The crate MUST provide a `trace` module with adapter-facing instrumentation wrappers that
wrap handler invocations in structured spans and emit lifecycle events covering: invocation
start, success, failure, and the compensation equivalents (`compensation_started`,
`compensation_completed`, `compensation_failed`), mapping to `InvocationTimelineEvent` variants.

- **Rationale**: Enables adapters to emit consistent timeline events without requiring SDK
  consumers to add any observability code.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

#### No Consumer-Visible Tracing

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-no-consumer-tracing`

Handler invocation and compensation methods MUST NOT emit any observability events or spans
directly. All instrumentation MUST be contained in the `trace` module and invisible to
SDK consumers.

- **Rationale**: FunctionHandler implementations remain clean and free of platform-specific observability
  wiring; adapters control the observability boundary.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

### 5.7 Adapter Conformance Suite

#### Conformance Test Suite

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-conformance-suite`

The crate MUST ship an adapter conformance test suite that every runtime adapter runs
against its `RuntimeAdapter` implementation. Coverage MUST include: invocation status
transitions, retry semantics, compensation triggering, suspension/resume visibility,
and error taxonomy.

- **Rationale**: With invocation, scheduling, retry, and compensation execution
  distributed across adapters under the parent gear's thin-host / fat-plugin
  architecture, the conformance suite is the load-bearing mechanism for guaranteeing
  uniform user-visible semantics across backends.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

### 5.8 RuntimeAdapter Trait

The `RuntimeAdapter` trait is the primary plugin contract: the host dispatches every
runtime concern to the adapter through this trait, and each adapter implements it using
its backend's native primitives. The SDK declares the trait and the value types that
cross the boundary; adapters provide the implementation. This requirement set captures
what the trait surface MUST expose; it does not mandate a specific method partitioning
across sub-traits — DESIGN §3.3 records the chosen split.

#### Invocation Method

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-adapter-invoke`

`RuntimeAdapter` MUST expose an async invocation method that accepts an
`InvocationRecord` from the host and drives the adapter's internal invocation bridge
(input deserialisation, `Context` construction, `Environment` population, handler call,
result/error mapping back to the host). The method MUST be cancellation-safe in the
sense expected by the adapter's backend.

- **Rationale**: The parent gear's thin-host / fat-plugin architecture makes invocation
  a per-plugin concern; this method is the single entry point the host uses to drive any
  backend.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`,
  `cpt-cf-serverless-runtime-plugin-sdk-actor-runtime-host`

#### Control Methods

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-adapter-control`

`RuntimeAdapter` MUST expose control methods covering cancel, suspend, and resume of an
in-flight invocation, addressed by `invocation_id`. The contract MUST tolerate
backend-native semantics: an adapter whose backend cannot suspend MAY return a typed
`ServerlessSdkError` variant indicating the operation is unsupported, and the
conformance suite (§5.7) verifies that the returned semantics are uniform.

- **Rationale**: Cancel/suspend/resume are user-visible operations exposed through the
  host's control plane; the host MUST be able to forward them to whichever adapter owns
  the invocation without leaking backend-specific call shapes.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`,
  `cpt-cf-serverless-runtime-plugin-sdk-actor-runtime-host`

#### Schedule Method

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-adapter-schedule`

`RuntimeAdapter` MUST expose an async schedule method that accepts a `Schedule` value
(cron/interval expression, missed-fire policy, concurrency, target function id, tenant,
correlation context) and binds it to the adapter's backend-native scheduler (Temporal
Schedule API, EventBridge Scheduler, Cloud Scheduler, Azure Durable timers, or the
shared in-process helper crate consumed by Starlark/WASM-style backends). Symmetric
methods for updating and revoking a binding MUST also be exposed.

- **Rationale**: Per the parent gear's thin-host / fat-plugin architecture, the host
  does not run a scheduler; the SDK normalises the schedule value type so any backend
  can map a uniform host request onto its native primitive.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`,
  `cpt-cf-serverless-runtime-plugin-sdk-actor-runtime-host`

#### Event-Trigger Method

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-adapter-event-trigger`

`RuntimeAdapter` MUST expose an async event-trigger method that accepts a `Trigger`
value and binds it to the adapter's backend-native event source (Temporal signals,
EventBridge + SQS, Eventarc, Azure Events, or the shared helper crate). Symmetric
methods for updating and revoking a binding MUST also be exposed.

- **Rationale**: Per the parent gear's thin-host / fat-plugin architecture, the host
  does not match events to triggers; each adapter does so using its backend's
  primitives, driven by a uniform binding type.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`,
  `cpt-cf-serverless-runtime-plugin-sdk-actor-runtime-host`

#### Schedule and Trigger Value Types

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-schedule-trigger-types`

The SDK MUST export `Schedule` and `Trigger` value types as `#[non_exhaustive]`
structs/enums covering the fields exchanged across the
`RuntimeAdapter::schedule` and `RuntimeAdapter::event_trigger` boundary. These types
MUST NOT carry adapter-specific or engine-specific fields; backend-specific options
(e.g., Temporal namespace, EventBridge ARN) are configured inside the adapter from its
own configuration source, not threaded through the SDK type.

- **Rationale**: A stable, engine-agnostic value type is the load-bearing contract for
  uniform schedule and event-trigger semantics across backends; engine-specific fields
  in the SDK would re-introduce the cross-cutting coupling the parent ADR rejected.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`,
  `cpt-cf-serverless-runtime-plugin-sdk-actor-runtime-host`

### 5.9 ServerlessRuntimeClient Trait

The `ServerlessRuntimeClient` trait is the inverse boundary: implemented by the host,
called by each adapter to keep the host's lightweight invocation index up to date and
to acknowledge schedule/trigger bindings. The host-indexed, plugin-detailed split — the
index is fed exclusively through events emitted via this trait — is recorded in the
parent gear's thin-host ADR.

#### Invocation-Index Event Emission

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-fr-runtime-client-index-events`

The SDK MUST declare `ServerlessRuntimeClient` with an async method by which an adapter
emits invocation-index updates carrying the fields the host indexes (invocation_id,
function_id, adapter, tenant, owner, status, timestamps, error summary). The method
MUST NOT require the adapter to expose its internal timeline, payloads, or
backend-native execution state — those remain plugin-owned.

- **Rationale**: The host runs aggregate queries off the index without fan-out to every
  registered adapter; the index is fed only by adapter-emitted events on this trait.
- **Actors**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`,
  `cpt-cf-serverless-runtime-plugin-sdk-actor-runtime-host`

---

## 6. Non-Functional Requirements

### 6.1 Gear-Specific NFRs

#### No Engine-Specific Dependencies

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-nfr-no-engine-deps`

The crate MUST NOT introduce any direct or transitive dependency on engine-specific crates
(`temporal-sdk`, `starlark`, any cloud FaaS SDK, or similar) at any point in its lifecycle.

- **Threshold**: Zero engine-specific crates in the dependency tree.
- **Rationale**: Enforces `cpt-cf-serverless-runtime-principle-impl-agnostic`; prevents
  accidental coupling that would break adapter portability.


#### Zero Unsafe Code

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-nfr-no-unsafe`

The crate MUST contain no `unsafe` blocks (enforced by `unsafe_code = "forbid"` in the
workspace lint configuration).

- **Threshold**: Zero `unsafe` blocks; workspace lint enforces this.
- **Rationale**: Safety guarantee for handler authors; no soundness risk from SDK internals.

#### Low Per-Invocation Overhead

- [ ] `p2` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-nfr-low-overhead`

The SDK's contribution to per-handler-invocation overhead MUST be minimal and predictable
for production async workloads.

- **Threshold**: The SDK's per-invocation instrumentation path MUST NOT introduce blocking I/O
  or synchronous computation. Overhead is limited to the async dispatch mechanism and one
  structured span emission per call (see DESIGN.md for rationale).
- **Rationale**: The SDK is on the critical invocation path for all adapters; latency overhead
  accumulates across high-frequency workloads.

#### Public API Documentation

- [ ] `p2` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-nfr-api-docs`

All public types, traits, and functions MUST have `rustdoc` documentation covering
purpose, usage, and any invariants or panics.

- **Threshold**: `cargo doc --no-deps` produces zero missing-documentation warnings;
  enforced by `#![deny(missing_docs)]` in CI.
- **Rationale**: Adapter Developers must be able to understand and implement the SDK
  contract from the documentation alone, without consulting DESIGN.md or engine
  internals. Aligns with UX-PRD-001 developer-experience target.
- **Verification Method**: `cargo doc --no-deps` in CI; zero missing-doc warnings.

#### Authoring Ergonomics

- [ ] `p2` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-nfr-authoring-ergonomics`

Adapter Developers MUST be able to implement `FunctionHandler` and `WorkflowHandler`
without knowledge of the SDK's internal async dispatch mechanism. The handler contracts
MUST NOT impose async machinery boilerplate on the implementor beyond writing the handler
logic itself.

- **Rationale**: The SDK's value proposition is that Adapter Developers focus on handler
  dispatch logic, not the underlying async infrastructure. Leaking internal dispatch
  concerns into the implementation contract undermines the developer experience target
  (UX-PRD-001).
- **Verification Method**: SDK examples and integration tests compile without requiring
  knowledge of the async dispatch mechanism; reviewed on every PR that touches handler contracts.

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
| Audit logging as security concern (SEC-PRD-004) | Addressed via §5.6 | Invocation lifecycle tracing events are emitted per §5.6 (trace gear). Security audit logging beyond invocation events is the adapter/runtime concern. |
| Privacy by design (SEC-PRD-005) | N/A | Internal developer tooling; no end-user PII collection. |
| Operational safety / fail-safe (SAFE-PRD-001, SAFE-PRD-002) | N/A | Pure library; no safety-critical operations, no physical systems interface, no hazards. |
| Monitoring / alerting / log retention (OPS-PRD-002) | N/A | Pure library; monitoring configuration is the adapter/runtime concern. The `trace` module provides the observability hooks; consumers configure retention. |
| UX accessibility — WCAG (UX-PRD-002) | N/A | No end-user UI; developer SDK only. |
| UX internationalization (UX-PRD-003) | N/A | No UI or user-facing strings. Error message strings are English-only; acceptable for internal developer tooling. |
| UX inclusivity (UX-PRD-005) | N/A | Developer SDK; no diverse end-user population considerations. |
| Support SLA / support tiers (MAINT-PRD-002) | N/A | Internal library; no external support tier. Issues tracked via the CF/Gears project repository. |
| Regulatory compliance — GDPR / HIPAA / PCI DSS (COMPL-PRD-001) | N/A | Internal Rust library; no regulatory obligations apply directly to this crate. |
| Industry certification standards (COMPL-PRD-002) | N/A | No formal certification required for an internal SDK crate. IEEE/ISO standards referenced in preamble are informational only. |
| Legal / ToS / consent requirements (COMPL-PRD-003) | N/A | Internal tool; no ToS, privacy policy, or consent flows. |
| Data ownership / stewardship (DATA-PRD-001) | N/A | The SDK passes data through as opaque blobs; ownership is the adapter/runtime concern. |
| Data quality requirements (DATA-PRD-002) | N/A | Pure library; no data storage or quality management. |
| Data lifecycle / retention / purge (DATA-PRD-003) | N/A | Pure library; no data persistence, retention, or archival. |

---

## 7. Public Library Interfaces

This crate exposes its public surface as Rust traits and types. For the concrete API
surface, stability classifications, and breaking change policies, see [DESIGN.md](./DESIGN.md).

### 7.1 Public API Surface

- Core handler authoring traits (function and workflow)
- Invocation context and environment access
- Typed error model with runtime error category mapping
- Adapter-facing instrumentation utilities

### 7.2 External Integration Contracts

- Invocation record → context mapping (deterministic field mapping)
- Compensation context → compensation input mapping (deterministic field mapping)
- Credstore → environment population (standard `CredStoreClientV1`-backed implementation)

---

## 8. Use Cases

### Adapter Developer Implements a FunctionHandler

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-usecase-impl-handler`

**Actor**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

**Preconditions**:
- `serverless-runtime-sdk` is a dependency of the adapter crate.
- The function's `IOSchema.params` and `IOSchema.returns` are known.

**Main Flow**:
1. Adapter Developer defines typed input and output types for the function.
2. Adapter Developer implements `FunctionHandler` on a handler struct, bridging the
   engine's execution model (e.g., wrapping a Starlark interpreter or Temporal activity).
3. The `FunctionHandler` receives typed input, reads invocation context and environment,
   and returns a typed result or a `ServerlessSdkError`.
4. The adapter deserialises params, dispatches the `FunctionHandler` via the instrumentation
   wrapper, and persists the result.

**Postconditions**:
- `InvocationRecord.result` contains the serialised output.
- An invocation span with `succeeded` event is emitted.

**Alternative Flows**:
- **`FunctionHandler` returns `UserError`**: `InvocationRecord.error` is set with
  `RuntimeErrorCategory::NonRetryable`; no retry is attempted.
- **`FunctionHandler` returns `Internal`**: `RuntimeErrorCategory::Retryable`; runtime
  may retry per the function's `RetryPolicy`.
- **Deadline exceeded**: `FunctionHandler` checks deadline via context, returns `Timeout`;
  mapped to `RuntimeErrorCategory::Timeout`.

### Adapter Developer Wires SDK into an Adapter Crate

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-usecase-wire-adapter`

**Actor**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

**Preconditions**:
- `serverless-runtime-sdk` is a dependency of the adapter crate.
- The adapter has access to an `InvocationRecord` and the runtime's `CompensationContext`.

**Main Flow**:
1. Adapter constructs a `Context` by mapping fields from `InvocationRecord` and `InvocationObservability`.
2. Adapter implements `Environment`, pre-fetching config and secret values before invocation.
3. Adapter resolves the handler instance for the requested function.
4. Adapter dispatches the invocation through the instrumentation wrapper for automatic tracing.
5. Adapter receives the result or error, maps the error variant to `RuntimeErrorCategory`, and persists the outcome in `InvocationRecord`.

**Postconditions**:
- `InvocationRecord` is updated with the serialised result or mapped error category.
- Lifecycle span events are emitted without any handler-side code.

**Alternative Flows**:
- **Compensation trigger**: Adapter constructs `CompensationInput` from `CompensationContext` and dispatches
  it through the compensation instrumentation wrapper on a `WorkflowHandler` instance.

---

### Adapter Developer Implements Workflow Compensation

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-plugin-sdk-usecase-impl-compensation`

**Actor**: `cpt-cf-serverless-runtime-plugin-sdk-actor-adapter-dev`

**Preconditions**:
- Adapter Developer has a `WorkflowHandler` implementation.
- Workflow failed or was canceled; `CompensationInput` is available.

**Main Flow**:
1. Adapter dispatches the `WorkflowHandler` via the compensation instrumentation wrapper.
2. The `WorkflowHandler` checks `input.original_workflow_invocation_id` for idempotency.
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

- [ ] A function handler can be implemented and unit-tested without depending on any adapter crate.
- [ ] A workflow handler can implement compensation logic using all contextual fields defined
      in the compensation model (as specified in DESIGN.md §3.1).
- [ ] The environment abstraction can be satisfied in handler unit tests without any platform
      infrastructure or async runtime.
- [ ] Every SDK error maps to a documented runtime error category; the public API is fully
      documented with no missing-doc warnings.
- [ ] Instrumentation emits lifecycle events for handler invocations without any
      observability code in the handler implementation.
- [ ] Instrumentation emits lifecycle events for compensation invocations without any
      observability code in the workflow handler implementation.

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

There are no open questions at this time.

---

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Trait signature changes break downstream adapters | High — any `FunctionHandler` impl stops compiling | At 0.x, breaking changes may occur in minor releases (0.y → 0.y+1); after 1.0, breaking changes require a semver major bump. Changelog documents all signature changes. |
| Engine-specific dep accidentally introduced via transitive pull | High — violates `cpt-cf-serverless-runtime-plugin-sdk-nfr-no-engine-deps` | Manual PR review; minimal dependency surface |
| `async-trait` boxing overhead at cold-path invocations | Low — per-invocation allocation in already-async context | Acceptable trade-off for stable ergonomics; revisit when RPITIT Send bound stabilises fully |

---

## 13. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **Serverless Runtime PRD**: [gears/serverless-runtime/docs/PRD.md](../../docs/PRD.md)
- **Serverless Runtime DESIGN**: [gears/serverless-runtime/docs/DESIGN.md](../../docs/DESIGN.md)
