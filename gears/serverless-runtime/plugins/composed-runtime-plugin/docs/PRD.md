# PRD — Composed Runtime Plugin


<!-- toc -->

- [1. Overview](#1-overview)
  - [Purpose](#purpose)
  - [Background / Problem Statement](#background--problem-statement)
  - [Goals (Business Outcomes)](#goals-business-outcomes)
  - [Glossary](#glossary)
- [2. Actors](#2-actors)
  - [Human Actors](#human-actors)
  - [System Actors](#system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
- [4. Scope](#4-scope)
  - [In Scope](#in-scope)
  - [Out of Scope](#out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [FR-001 Embedded-Executor Extensibility](#fr-001-embedded-executor-extensibility)
  - [FR-002 GTS-Keyed In-Plugin Router](#fr-002-gts-keyed-in-plugin-router)
  - [FR-003 Unified `ExecutionContext`](#fr-003-unified-executioncontext)
  - [FR-004 Durable Checkpoint Store with Per-Executor Schemas](#fr-004-durable-checkpoint-store-with-per-executor-schemas)
  - [FR-005 Event Hub for Async Suspension and Progress Notifications](#fr-005-event-hub-for-async-suspension-and-progress-notifications)
  - [FR-006 Sync and Async Operational Shapes](#fr-006-sync-and-async-operational-shapes)
  - [FR-007 Managed Deployment Modes (In-Process and Managed-OoP)](#fr-007-managed-deployment-modes-in-process-and-managed-oop)
  - [FR-008 SDK Plugin Trait Implementation](#fr-008-sdk-plugin-trait-implementation)
  - [FR-009 Retry, Compensation, Timeout Enforcement](#fr-009-retry-compensation-timeout-enforcement)
  - [FR-010 Multi-Tenant Scoping at the Router](#fr-010-multi-tenant-scoping-at-the-router)
  - [FR-011 Single Trace and Debug Plane](#fr-011-single-trace-and-debug-plane)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [NFR-001 Performance — In-Process Dispatch](#nfr-001-performance--in-process-dispatch)
  - [NFR-002 Performance — Managed-OoP Dispatch](#nfr-002-performance--managed-oop-dispatch)
  - [NFR-003 Reliability — Replay Correctness](#nfr-003-reliability--replay-correctness)
  - [NFR-004 Tenant Isolation](#nfr-004-tenant-isolation)
  - [NFR-005 Hot Reload — Hot-Loadable Embedded Executors](#nfr-005-hot-reload--hot-loadable-embedded-executors)
  - [NFR-006 Observability](#nfr-006-observability)
  - [NFR-007 Reuse of ModKit Primitives](#nfr-007-reuse-of-modkit-primitives)
  - [NFR-008 Single-Binary Local Dev](#nfr-008-single-binary-local-dev)
  - [NFR Exclusions](#nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [Public API Surface](#public-api-surface)
  - [External Integration Contracts](#external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [UC-001 Compose a Starlark Workflow with a Native Rust Action](#uc-001-compose-a-starlark-workflow-with-a-native-rust-action)
  - [UC-002 Run an Untrusted Customer-Supplied Starlark Callable in Isolation](#uc-002-run-an-untrusted-customer-supplied-starlark-callable-in-isolation)
  - [UC-003 Event-Triggered Async Workflow with Suspend/Resume](#uc-003-event-triggered-async-workflow-with-suspendresume)
  - [UC-004 Hot-Reload a Native Rust Callable Without Host Restart](#uc-004-hot-reload-a-native-rust-callable-without-host-restart)
  - [UC-005 Add a New Embedded Language to the Plugin](#uc-005-add-a-new-embedded-language-to-the-plugin)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

**Module:** `cyberware-composed-runtime-plugin`
**Parent module:** `cyberware-serverless-runtime` (host) — see [serverless-runtime PRD](../../../docs/PRD.md)
**ID prefix:** cpt-cf-composed-runtime-plugin-{kind}-{slug}

## 1. Overview

### Purpose

Provide a single runtime plugin under the Serverless Runtime thin-host model ([serverless-runtime ADR-0005](../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)) that hosts multiple **embedded language executors** — Starlark today, native Rust today, and future embedded languages such as CEL — on a shared in-plugin environment (GTS-keyed router, unified `ExecutionContext`, durable checkpoint store, eventing) so that callables written in different embedded languages can compose freely and inherit durability, replay, tracing, tenant scoping, and event-driven suspend/resume for free.

The plugin is delivered in two operational modes from a single binary:

- **In-process mode (default).** The plugin runs as a ModKit module inside the host process; embedded callables dispatch through direct function calls with `Arc`-cloned `ExecutionContext` handles.
- **Managed-out-of-process mode (per-callable opt-in).** The same plugin binary is supervised as a child process by the existing ModKit OoP backend; the host plugin's in-process instance routes selected callables to the child via `modkit-transport-grpc` resolved through `ClientHub`. Caller code is unchanged.

### Background / Problem Statement

The Serverless Runtime platform allows tenants to author and invoke functions and workflows at runtime ([serverless-runtime PRD](../../../docs/PRD.md)). The thin-host architecture ([serverless-runtime ADR-0005](../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)) places the invocation engine, scheduler, event-trigger handling, retry, compensation, and checkpoint storage inside **runtime plugins**, with the host owning only the registry, REST/JSON-RPC/MCP surfaces, GTS validation, audit aggregation, plugin dispatch, tenant policy, and a lightweight invocation index.

Within that thin-host model, code-level callables (Starlark, native Rust, future CEL, future Wasm) have a shared set of needs: a router for cross-callable invocation, a checkpoint store, an eventing mechanism, replay-aware execution context, tenant scoping, tracing, and a path to isolation for untrusted code. If each embedded language were its own plugin, each would re-implement that shared core; cross-language composition would degrade into per-pair custom glue. The Composed Runtime plugin solves this by being the single plugin that hosts **all** embedded language executors against one shared environment ([ADR-0001](ADR/0001-cpt-cf-composed-runtime-plugin-adr-composed-runtime.md)).

This plugin is the focus runtime for the platform. External orchestrators — Temporal ([serverless-runtime ADR-0003](../../../docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md), [serverless-runtime ADR-0004](../../../docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md)), cloud FaaS bridges (Lambda, Cloud Functions, Azure Functions), and third-party engines — are out of scope for this plugin; when delivered, they ship as separate fat plugins under the same thin-host model.

### Goals (Business Outcomes)

- **Single environment for code-level callables.** Reduce the cost of adding a new embedded language to one `EmbeddedExecutor` implementation and one registration call; checkpointing, eventing, routing, tracing, and tenant scoping come for free.
- **Free composition across embedded executors.** A Starlark workflow invokes a native Rust callable as a step; a native Rust callable invokes a Starlark callable for application-specific logic; future embedded languages slot in without changing any existing caller.
- **Operational shapes covered.** API-driven request/response, event-driven async, scheduled, and stateful workflow shapes are all served by the same plugin instance with one durability story.
- **Two deployment modes, one binary.** In-process by default (zero IPC tax for trusted code); managed-out-of-process opt-in for untrusted, resource-greedy, or fault-prone callables — no caller-side changes.
- **Host stays thin.** The plugin emits index/timeline events to the host through the `ServerlessRuntimeClient` and otherwise remains opaque; the host has no compile-time dependency on this plugin.

### Glossary

| Term | Definition |
|------|------------|
| **Host** | The serverless-runtime host module (`cyberware-serverless-runtime`). Owns Registry, Tenant Policy, REST/JSON-RPC/MCP surfaces, GTS validation, audit aggregation, plugin dispatch, lightweight invocation index. See [serverless-runtime DESIGN §1.4](../../../docs/DESIGN.md#14-modkit-integration). |
| **Plugin / fat plugin** | A runtime plugin under [serverless-runtime ADR-0005](../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md) that owns the full execution stack (invocation engine, scheduler, event-trigger handling, retry, compensation, checkpoint store) and implements the `RuntimeAdapter` trait. |
| **Composed Runtime plugin** | This plugin. Hosts embedded language executors against a shared environment. |
| **Embedded executor** | An implementation of the in-plugin `EmbeddedExecutor` trait for one language/runtime (Starlark, native Rust, future CEL, etc.) that runs inside the Composed Runtime plugin. |
| **Callable** | A registered function or workflow definition (GTS-identified) that is dispatched to an embedded executor through the in-plugin router. |
| **In-plugin router** | The plugin-local GTS-keyed dispatcher resolving callable IDs to embedded executors. Distinct from the host's plugin-dispatch (which routes plugin GTS IDs to plugins). |
| **`ExecutionContext`** | The unified per-invocation context exposed to every embedded callable: checkpoint API, event-wait API, sync+async invocation API, tenant/security/trace surfaces. |
| **In-process mode** | Embedded executors run in the host process via direct trait calls. |
| **Managed-OoP mode** | The plugin runs the same binary as a child process supervised by the ModKit OoP backend; selected callables are routed to it via `modkit-transport-grpc`. |
| **SDK** | The serverless-runtime contract crate (`serverless-runtime-sdk`) that defines the plugin trait, host trait (event port), domain types, and SDK error taxonomy. |

## 2. Actors

### Human Actors

- **Callable authors** — write callables in an embedded language (Starlark today, native Rust today). Author against the language-specific surface (e.g. Starlark `ctx.*` helpers, the native Rust SDK proc-macro); they do not interact with this plugin's internal contracts directly.
- **Platform operators** — configure deployment mode per callable (in-process vs. managed-OoP); roll out hot-loaded native libraries; observe plugin health and per-callable metrics surfaced through the host.

### System Actors

- **Serverless Runtime host module** — dispatches invocation/scheduling/trigger operations into this plugin via the `RuntimeAdapter` trait; receives index and timeline events from this plugin via the `ServerlessRuntimeClient`.
- **Embedded executor implementations** — Starlark ([ADR-0002](ADR/0002-cpt-cf-composed-runtime-plugin-adr-starlark-runtime.md)), native Rust ([ADR-0003](ADR/0003-cpt-cf-composed-runtime-plugin-adr-native-rust-executor.md)), and any future embedded language (e.g. CEL). Each is a separate crate that depends on the Composed Runtime plugin's internal SDK to obtain the `EmbeddedExecutor` trait and `ExecutionContext` types.
- **ModKit OoP supervisor** — spawns and supervises the managed-OoP variant of this plugin when configured.
- **Event broker (via runtime services)** — delivers external events to suspended invocations through the in-plugin Event Hub.
- **Outbound API gateway (via runtime services)** — receives outbound HTTP calls that callables initiate through `ExecutionContext`.

## 3. Operational Concept & Environment

The Composed Runtime plugin is delivered as part of the platform binary and runs in two operational contexts that share the same code path:

- **Embedded in the platform process.** The plugin is loaded as a ModKit module at platform startup. Each embedded executor's lifecycle is tied to the plugin's module lifecycle. Local development, single-binary deployments, and trusted-code paths use this configuration by default.
- **Managed-out-of-process per callable.** Operators or tenant policy mark selected callables for managed-OoP deployment. The platform spawns a child-process instance of the same plugin binary under the ModKit OoP backend (`LocalProcessBackend` for self-managed, `K8sBackend` for cluster, `StaticBackend` for pre-provisioned). The child process exposes the embedded executor over `modkit-transport-grpc`; the in-process plugin instance's router resolves to a gRPC client via `ClientHub` and dispatches transparently.

The plugin assumes:

- The host process provides the standard ModKit primitives — `ClientHub`, `OopBackend`, `DirectoryService`, `modkit-transport-grpc`, `modkit-db`.
- A platform event broker is reachable for `wait_event` delivery and event-trigger firings.
- An outbound API gateway routes egress HTTP calls from callables.
- A GTS schema registry holds callable IDs, params/returns schemas, event-type IDs, and per-executor checkpoint schema IDs.

The plugin does **not** open external listening ports of its own (the host owns REST/JSON-RPC/MCP); it communicates only over in-process Rust traits and, for managed-OoP, over `modkit-transport-grpc` gRPC channels supervised by the OoP backend.

## 4. Scope

### In Scope

- Implementation of the `RuntimeAdapter` trait from `serverless-runtime-sdk`.
- In-plugin GTS-keyed router for embedded callables.
- Unified `ExecutionContext` with checkpoint, event-wait, progress-emit, sync+async invocation, identity, and trace surfaces.
- Durable checkpoint envelope store with per-executor schemas.
- Plugin-local Event Hub for `wait_event` / schedule / event-trigger delivery.
- Per-callable deployment mode (in-process / managed-OoP variants).
- Retry, compensation, timeout enforcement inside this plugin.
- Hosting the Starlark embedded executor ([ADR-0002](ADR/0002-cpt-cf-composed-runtime-plugin-adr-starlark-runtime.md)) and the native Rust embedded executor ([ADR-0003](ADR/0003-cpt-cf-composed-runtime-plugin-adr-native-rust-executor.md)).
- An internal `EmbeddedExecutor` trait surface stable enough to host a future CEL embedded executor without plugin-core changes.

### Out of Scope

- Deep integration with external orchestrators — Temporal ([serverless-runtime ADR-0003](../../../docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md), [serverless-runtime ADR-0004](../../../docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md)), cloud FaaS bridges (Lambda, Cloud Functions, Azure Functions), or third-party engines. These ship as **separate fat plugins** under the thin-host model.
- The host's REST / JSON-RPC / MCP surfaces, function registry, tenant policy storage, audit aggregation, and lightweight invocation index — those belong to the host module.
- Defining the Serverless Workflow DSL or a Temporal worker.
- Authoring tools (IDE, linting, formatting) for the embedded languages.
- Marketplace / signing / distribution for customer-supplied hot-loaded libraries; this PRD specifies the loader contract via [ADR-0003](ADR/0003-cpt-cf-composed-runtime-plugin-adr-native-rust-executor.md) but defers distribution to a later ADR.
- The platform security model (execution identity, secret references, privilege scoping); see `NEXT_ADR_SCOPE.md` ADR-6.

## 5. Functional Requirements

### FR-001 Embedded-Executor Extensibility

The plugin **shall** define a single internal `EmbeddedExecutor` trait that any embedded language implementation can satisfy, and **shall** allow a new embedded executor (Starlark, native Rust, future CEL, future scripting language) to be added by:

- providing one `impl EmbeddedExecutor`,
- registering it with the plugin at module init,
- declaring its checkpoint schema ID.

Adding a new embedded executor **shall not** require changes to the plugin's router, `ExecutionContext`, checkpoint store, event hub, tenant/security pipeline, or to any other embedded executor.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-fr-embedded-executor-extensibility`

### FR-002 GTS-Keyed In-Plugin Router

The plugin **shall** maintain a callable registry keyed by GTS callable ID and **shall** dispatch any embedded callable through `Router::invoke(callable_id, request, ctx)`. The signature **shall** be identical regardless of which embedded executor implements the callable and regardless of whether the callable runs in-process or in the plugin's managed-OoP variant.

Cross-plugin invocations (e.g. into a callable hosted in a separate plugin) **shall not** be served by this router and **shall not** be reachable through `ctx.invoke` at all: `ExecutionContext::invoke` rejects an unknown callable with a "callable not found" error. The SDK defines no plugin→host outbound-routing path; cross-plugin composition, when needed, is a host-orchestration concern arranged above this plugin.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-fr-in-plugin-router`

### FR-003 Unified `ExecutionContext`

The plugin **shall** provide a single `ExecutionContext` to every embedded callable invocation, exposing:

- **Identity surfaces:** `invocation_id`, `callable_id`, `tenant_id`, `security_context`, `trace`.
- **Replay surfaces:** `is_replay()`, `attempt()`, `read_checkpoint(label)`, `write_checkpoint(label, schema_id, payload)`.
- **Eventing surfaces:** `wait_event(filter)`, `emit_progress(payload)`.
- **Sync + async invocation:** `invoke(callee_id, request)` dispatched through the in-plugin router; the same surface serves request/response, event-triggered, scheduled, and workflow shapes.
- **Service handles:** typed access to plugin-managed services (HTTP via outbound API gateway, connection pools, etc.) through `ClientHub`-resolved interfaces.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-fr-execution-context`

### FR-004 Durable Checkpoint Store with Per-Executor Schemas

The plugin **shall** persist checkpoint envelopes to a durable store, one row per `(invocation_id, label)`, with columns `schema_id`, `payload` (opaque bytes), `attempt`, `created_at`. The plugin **shall not** interpret payload bytes; each embedded executor owns the GTS schema ID for its checkpoint payload and is responsible for serialization/deserialization.

Checkpoints written by one embedded executor **shall** be retrievable on resume in the same `(invocation_id, label)` regardless of in-process / managed-OoP mode change between write and read.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-fr-checkpoint-store`

### FR-005 Event Hub for Async Suspension and Progress Notifications

The plugin **shall** integrate with the platform event broker through a plugin-local Event Hub that:

- delivers events matching `ExecutionContext::wait_event(filter)` to suspended invocations,
- emits `ExecutionContext::emit_progress(payload)` notifications as timeline events through the `ServerlessRuntimeClient` to the host's invocation index,
- supports schedule firings and event-trigger firings as the same suspend/resume mechanism as `wait_event`.

An invocation **shall** be able to suspend on `wait_event` indefinitely (bounded only by tenant policy) and resume from its last checkpoint envelope when the event arrives.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-fr-event-hub`

### FR-006 Sync and Async Operational Shapes

The plugin **shall** serve four operational shapes through one router, one `ExecutionContext`, and one checkpoint store:

- **API-driven (sync request/response).** A callable invoked through the host's REST/JSON-RPC/MCP surface runs to completion in the same dispatch and returns a response.
- **Event-driven (async).** A callable triggered by event delivery or schedule firing dispatches into the plugin and may suspend / resume via `wait_event` or `read_checkpoint` / `write_checkpoint`.
- **Scheduled.** Schedule firings (cron / interval) dispatch as invocations through the same router; missed-schedule and concurrency policies are honored.
- **Stateful workflow.** A callable with workflow traits runs across multiple suspend/resume cycles, fanning out child invocations via `ctx.invoke`, waiting on correlated events via `ctx.wait_event`, and persisting state at every safe point.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-fr-operational-shapes`

### FR-007 Managed Deployment Modes (In-Process and Managed-OoP)

The plugin **shall** support per-callable deployment-mode selection:

- `in_process` (default) — callable runs in the host process.
- `managed_oop_local_process`, `managed_oop_k8s`, `managed_oop_static` — callable runs in a child-process instance of the same plugin binary, supervised by the matching ModKit OoP backend, reached over `modkit-transport-grpc`.

Caller code **shall not** be aware of the deployment mode; the in-plugin router resolves it transparently through `ClientHub`. Mode is declared at callable registration time or by tenant policy. A change in mode **shall** require re-registration and, for managed-OoP modes, a child-process restart.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-fr-deployment-modes`

### FR-008 SDK Plugin Trait Implementation

The plugin **shall** implement the `RuntimeAdapter` trait defined in `serverless-runtime-sdk`, covering at least:

- Callable registration / deregistration / lookup,
- Invocation lifecycle (start, suspend, resume, cancel),
- Scheduling (cron / interval),
- Event-trigger handling,
- Tenant policy application,
- Health / readiness reporting to the host,
- Timeline event emission to the host's invocation index via the `ServerlessRuntimeClient`.

The plugin **shall not** expose host APIs of its own; all external interaction goes through the host or through the plugin's own `RuntimeAdapter` trait surface.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-fr-sdk-plugin-trait`

### FR-009 Retry, Compensation, Timeout Enforcement

The plugin **shall** implement, inside its own runtime:

- Retry of failed invocations or steps per policy declared on the callable / tenant.
- Step-level compensation when an embedded executor exposes a `compensate` hook.
- Function-level compensation on terminal failure per the WorkflowTraits model in the host DESIGN.
- Wall-clock and resource-limit enforcement at invocation and step granularity.

The host **shall not** be involved in any of these mechanics beyond receiving timeline events.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-fr-retry-compensation-timeouts`

### FR-010 Multi-Tenant Scoping at the Router

The plugin **shall** apply the calling `tenant_id` / `security_context` to every embedded-executor dispatch through the in-plugin router. Embedded executors **shall not** need to enforce tenant scoping themselves; they read it from the supplied `ExecutionContext`.

Cross-tenant invocation **shall** be rejected at the router unless tenant policy explicitly allows it (e.g. system-owned utility callables).

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-fr-tenant-scoping`

### FR-011 Single Trace and Debug Plane

The plugin **shall** emit one OpenTelemetry span per dispatch through the in-plugin router, regardless of embedded executor or in-process / managed-OoP boundary. Spans **shall** carry context propagated over `modkit-transport-grpc` for managed-OoP hops.

Per-invocation timeline events **shall** be emitted to the host's invocation index through the `ServerlessRuntimeClient` for every checkpoint write, every `wait_event` suspend/resume, every cross-callable `invoke`, and every status transition.

The plugin **shall** expose its trace stream as the substrate for the future debug ADR (breakpoints, step-through); the debug subsystem will be a router subscriber inside this plugin.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-fr-trace-debug-plane`

## 6. Non-Functional Requirements

### NFR-001 Performance — In-Process Dispatch

In-plugin router dispatch **shall** have p95 latency under **10 µs** for in-process callables (function call + lock-free callable-registry read + `Arc` clone of `ExecutionContext`), excluding executor-specific work.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-nfr-dispatch-performance`

### NFR-002 Performance — Managed-OoP Dispatch

Managed-OoP router dispatch **shall** be dominated by `modkit-transport-grpc` round-trip time and **shall not** add more than **200 µs** of overhead on top of the gRPC RTT (request serialization + framing + tracing context propagation).

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-nfr-oop-dispatch-overhead`

### NFR-003 Reliability — Replay Correctness

A workflow-shaped invocation suspended via `wait_event` or `write_checkpoint` **shall** resume on a different host process / managed-OoP child if necessary, replaying from its last checkpoint envelope, and **shall not** lose progress or duplicate side-effecting operations that were committed before suspension.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-nfr-replay-correctness`

### NFR-004 Tenant Isolation

The plugin **shall** enforce tenant-scoped queries on its callable registry, checkpoint store, and timeline event emission. Untrusted callables **shall** be runnable in the plugin's managed-OoP variant so that a panic, OOM, or runaway loop in a single callable cannot affect other tenants' callables or the host process.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-nfr-tenant-isolation`

### NFR-005 Hot Reload — Hot-Loadable Embedded Executors

For embedded executors that support hot reload (today: native Rust per [ADR-0003](ADR/0003-cpt-cf-composed-runtime-plugin-adr-native-rust-executor.md)), the plugin **shall** drain in-flight invocations from the outgoing library version, load the new version, and resume any drained invocations from their checkpoint envelopes without restarting the host process and without leaking threads / file descriptors / memory mappings.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-nfr-hot-reload`

### NFR-006 Observability

The plugin **shall** expose, via the standard ModKit observability surface:

- Per-callable invocation counts, durations, and outcomes (success, retry, suspend, resume, cancel, fail).
- Per-tenant aggregates of the above.
- Plugin-level metrics: in-flight invocation count, suspended invocation count, checkpoint-store row count, event-hub subscription count.
- Health and readiness state.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-nfr-observability`

### NFR-007 Reuse of ModKit Primitives

The plugin **shall not** introduce a parallel IPC, RPC, supervision, or directory mechanism. The managed-OoP variant **shall** be built on:

- `ClientHub` for type-safe cross-callable dispatch,
- `OopBackend` / `LocalProcessBackend` / K8s / Static backends for child-process supervision,
- `bootstrap::oop::run_oop_with_options` for managed-OoP bootstrap,
- `modkit-transport-grpc` for cross-process transport,
- `DirectoryService` for module discovery,
- `modkit-db` for the durable checkpoint store.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-nfr-modkit-reuse`

### NFR-008 Single-Binary Local Dev

The plugin **shall** be runnable as part of a single host binary in local-dev configuration, with all embedded executors loaded in-process and no IPC. Adding embedded executors **shall not** require operating additional processes for the simplest scenarios.

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-nfr-single-binary-dev`

### NFR Exclusions

The following non-functional requirement areas are explicitly **not addressed** by this plugin and inherit their treatment from the platform, the host module, or future ADRs:

- **Authentication / authorization of platform callers.** Owned by the host module's API surfaces (REST/JSON-RPC/MCP) and the future security-model ADR (`NEXT_ADR_SCOPE.md` ADR-6).
- **Secret management.** Secret references and binding model belong to the platform-wide security ADR; the plugin only consumes resolved values through `ExecutionContext`-provided services.
- **Long-term retention / archival of checkpoint envelopes and timeline events.** Retention policy is a tenant-policy concern owned by the host; the plugin enforces what the host configures.
- **Encryption at rest / in transit of checkpoint payloads.** Inherited from `modkit-db` and the platform networking layer; this plugin does not implement bespoke encryption.
- **Distributed tracing backend selection.** The plugin emits OpenTelemetry spans; the backend (Jaeger, Tempo, etc.) is a platform-deployment concern.
- **API/UI for managing callables.** The host owns these surfaces; the plugin has no first-party UI.

## 7. Public Library Interfaces

### Public API Surface

The plugin exposes **no first-party public API** outside the ModKit module boundary. All inbound interaction reaches the plugin through the `RuntimeAdapter` trait defined in `serverless-runtime-sdk`, which the host invokes via its plugin-dispatch.

The plugin's outbound surfaces inside the platform (consumed only by embedded-executor crates that ship alongside this plugin) are the internal `EmbeddedExecutor` trait and the `ExecutionContext` trait. These are deliberately private to the plugin's internal SDK (working name `cyberware-composed-runtime-embedded-sdk`) and **not** considered a stable public library interface.

### External Integration Contracts

The plugin participates in the following platform-level integration contracts. Each is a contract owned by another module or library; this plugin **consumes** these contracts and emits events through them rather than defining its own external API.

- **`RuntimeAdapter` trait** — `serverless-runtime-sdk` (TBD). Inbound. Defines callable registration, invocation lifecycle, scheduling, event-trigger handling, health, and the event port for timeline emission. The plugin satisfies this trait.
- **`ServerlessRuntimeClient`** — same crate as above. Outbound. The plugin publishes index / timeline events to the host through this port.
- **`ClientHub` registration** — `libs/modkit/src/client_hub.rs`. Bidirectional. The plugin registers its `EmbeddedExecutor` implementations under `ClientScope::gts_id(callable_id)` and resolves OoP-bound embedded executors through the same registry.
- **OoP backend** — `libs/modkit/src/bootstrap/oop.rs` + `libs/modkit/src/backends/`. Outbound (the plugin requests child-process supervision). Used by the managed-OoP variant.
- **`modkit-transport-grpc`** — `libs/modkit-transport-grpc/`. Bidirectional. Carries `EmbeddedExecutor::invoke` calls and `ExecutionContext` operations across the managed-OoP boundary, with OpenTelemetry context propagation.
- **`modkit-db`** — `libs/modkit-db/`. Outbound. Persists the checkpoint envelope store and plugin-internal lifecycle / subscription state.
- **Event broker (resolved via `ClientHub`)** — Bidirectional. Publishes progress notifications; subscribes for `wait_event` delivery and event-trigger firings.
- **Outbound API gateway (resolved via `ClientHub`)** — Outbound. Routes HTTP egress from callables.
- **GTS schema registry** — Outbound. The plugin reads callable IDs, params/returns schemas, event-type IDs, and per-executor checkpoint schema IDs.

## 8. Use Cases

### UC-001 Compose a Starlark Workflow with a Native Rust Action

A platform tenant author writes a Starlark workflow that orchestrates a multi-step business process. One of the steps is implemented as a high-throughput native Rust callable (e.g. crypto verification, format parsing). The Starlark workflow invokes the native callable via `ctx.invoke(native_callable_id, input)`. The Composed Runtime plugin resolves the native callable through its in-plugin router, dispatches it in-process (default), and returns the result to the Starlark code under one trace and one tenant context. Checkpoints from both executors are persisted under the same `invocation_id` with their respective schemas.

### UC-002 Run an Untrusted Customer-Supplied Starlark Callable in Isolation

A platform operator marks a tenant-uploaded Starlark callable as `managed_oop_local_process`. At registration time, the plugin requests the OoP backend to spawn a child-process instance of itself; the child loads the Starlark interpreter inside that process. Subsequent invocations of this callable are routed by the in-plugin router through a `modkit-transport-grpc` client to the child process. A panic, OOM, or runaway loop inside the customer code is contained by the OS process boundary and triggers a supervisor restart; the platform host process and all other tenants are unaffected.

### UC-003 Event-Triggered Async Workflow with Suspend/Resume

An event-trigger on an external system causes an event to be published to the platform event broker. The host's trigger handler dispatches into this plugin with the event payload. A Starlark workflow callable runs, performs an initial step, persists a checkpoint via `ctx.write_checkpoint`, and suspends on `ctx.wait_event` to await an approval event. Hours later, the approval event arrives at the Event Hub; the workflow resumes from its checkpoint with `is_replay() == true`, completes, and emits its terminal timeline event to the host.

### UC-004 Hot-Reload a Native Rust Callable Without Host Restart

A platform operator ships a new version of a native Rust callable library. The plugin's loader is invoked with the new artifact path; it loads the new library into a shadow slot, validates the ABI, drains in-flight invocations from the old library (letting them checkpoint and resume against the new library's implementation), then closes the old library. The host process continues serving traffic for every other callable throughout the operation; no other tenant or callable is affected.

### UC-005 Add a New Embedded Language to the Plugin

A platform engineer adds support for a new embedded language (e.g. CEL). The engineer creates one new crate that depends on the plugin's internal SDK and implements the `EmbeddedExecutor` trait, declaring the CEL checkpoint schema ID. At plugin init, the new executor is registered. From that point on, CEL callables are dispatched by the same in-plugin router, receive the same `ExecutionContext` (with all checkpoint, eventing, tenant, trace primitives intact), and compose with Starlark and native Rust callables via `ctx.invoke` — without changes to plugin core, host, or any existing embedded executor.

## 9. Acceptance Criteria

- **AC-001 Composability test.** A Starlark callable invokes a native Rust callable through `ctx.invoke`; the chain succeeds end-to-end with one trace, one tenant context, and per-executor checkpoint envelopes attributable per writer. (Maps to UC-001.)
- **AC-002 Managed-OoP transparency test.** The same callable definition is exercised once with `mode: in_process` and once with `mode: managed_oop_local_process`. Caller code and request/response shapes are identical; only latency differs. (Maps to UC-002.)
- **AC-003 Suspend/resume across process restart.** A workflow callable is suspended via `wait_event`; the host process is restarted; the event is delivered; the workflow resumes from its last checkpoint with `is_replay() == true` and completes successfully without duplicating committed side effects. (Maps to UC-003, NFR-003.)
- **AC-004 Hot reload of native library.** A native Rust callable library is reloaded under sustained traffic; pre-reload invocations either complete on the old version or resume on the new version via checkpoint; `/proc/self/maps` and a leak detector confirm no thread / file-descriptor / mapping leaks. (Maps to UC-004, NFR-005.)
- **AC-005 New-executor extensibility.** A stub `EmbeddedExecutor` for a placeholder language is added by a single new-crate change plus a single registration line; no plugin-core file changes are required; the placeholder language's callables flow through router, checkpointing, eventing, and tracing identically to Starlark / native Rust. (Maps to UC-005, FR-001.)
- **AC-006 In-process dispatch performance.** p95 in-plugin router dispatch latency for an in-process callable is under 10 µs over a 60-second steady-state load. (Maps to NFR-001.)
- **AC-007 Managed-OoP dispatch overhead.** p95 router overhead beyond gRPC RTT for a managed-OoP callable is under 200 µs over a 60-second steady-state load. (Maps to NFR-002.)
- **AC-008 Tenant isolation under failure.** A managed-OoP callable is induced to crash; other tenants' in-flight callables are unaffected; OoP backend restarts the child; failed invocations resume or are surfaced as terminal errors per policy. (Maps to NFR-004.)
- **AC-009 Single-binary local dev.** The full plugin with all embedded executors runs in-process as part of a single host binary, with no extra processes required, and serves every UC above modulo the explicit OoP isolation tests. (Maps to NFR-008.)

## 10. Dependencies

| Dependency | Form | Status |
|------------|------|--------|
| `serverless-runtime-sdk` (the host SDK contract crate) | Compile-time crate dependency; the plugin trait this plugin implements | **Not yet created** — referenced by [serverless-runtime ADR-0005](../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md); see Open Questions |
| `cyberware-serverless-runtime` (the host module) | Runtime dispatch caller (resolves this plugin via `ClientHub` by plugin GTS type) | **Not yet created** |
| ModKit primitives | `ClientHub`, `OopBackend`, `bootstrap::oop`, `DirectoryService`, `modkit-transport-grpc`, `modkit-db` | Exists in `libs/modkit/`, `libs/modkit-transport-grpc/`, `libs/modkit-db/` |
| Embedded executor crates | Starlark (per ADR-0002), native Rust SDK (per ADR-0003) | **Not yet created** — depend on this plugin's internal SDK |
| Outbound API gateway | Reached via `ClientHub` for HTTP egress from callables | Exists |
| Event broker | Reached via `ClientHub` for `wait_event` and trigger delivery | Exists |
| GTS schema registry | For checkpoint schema IDs, callable IDs, event type IDs | Exists |

## 11. Assumptions

- The thin-host SDK (`serverless-runtime-sdk`) will be designed and built ahead of, or alongside, this plugin and will follow the SDK pattern documented in [serverless-runtime DESIGN §1.4](../../../docs/DESIGN.md#14-modkit-integration).
- ModKit's existing `ClientHub` / OoP backend / `modkit-transport-grpc` / `DirectoryService` stack is sufficient for the managed-OoP variant without modification.
- `modkit-db` is the persistence layer for the checkpoint envelope store; no separate datastore is required.
- The platform's GTS schema registry will host the per-executor checkpoint schema IDs without this plugin owning a separate schema registry.
- Outbound HTTP is routed through the platform's outbound API gateway; this plugin does not perform direct egress.

## 12. Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| Internal `EmbeddedExecutor` trait churns as new languages are added, breaking Starlark / native Rust adapters | Medium | Version the internal trait; require deprecation windows; conformance test suite covers every embedded executor against the latest stable version |
| Checkpoint schema fragmentation (each embedded executor evolves its own schema independently) | Medium | Schemas are GTS-identified and versioned; the plugin enforces schema-ID match on `read_checkpoint`; cross-executor reads are not permitted |
| Hot-reload of native libraries leaks resources, requiring host restart | High (operational) | Strict no-static-state rule on native callables enforced by SDK proc-macro lints; reload test suite verifies via `/proc/self/maps` and leak detector |
| Managed-OoP child-process supervisor instability under high churn | Medium | Reuse ModKit's already-operating OoP backend; do not invent a new supervisor |
| Replay correctness gaps when an embedded executor changes its checkpoint schema across versions | High | Per-version schema IDs; schema migrations are an executor responsibility, not a plugin one |
| Composability illusion — author thinks cross-plugin `ctx.invoke` to a DSL/Temporal callable works through this plugin's router | Low | `ctx.invoke` is documented as in-plugin-only ([ADR-0001](ADR/0001-cpt-cf-composed-runtime-plugin-adr-composed-runtime.md) Non-goals); an unknown callee is rejected with a clear "callable not found" error rather than silently bridged. Cross-plugin composition is a host-orchestration concern, not reachable from inside a callable. |

## 13. Open Questions

- **SDK crate ownership.** Should `serverless-runtime-sdk` be designed and built first (as a prerequisite for this plugin), in parallel, or by the same effort that builds this plugin? Affects sequencing.
- **Internal SDK for embedded executors.** Should the `EmbeddedExecutor` trait, `ExecutionContext`, and `CheckpointEnvelope` live in this plugin's lib crate (private API), or in a small internal SDK crate (`cyberware-composed-runtime-embedded-sdk`) so that the Starlark / native Rust embedded-executor crates can depend on it without depending on the whole plugin? Likely the latter.
- **Embedded-executor registration model.** Compile-time registration of Starlark + native Rust at plugin init? Or dynamic registration at runtime (Starlark always; native Rust via hot-loaded libraries; future CEL via … ?). Currently leaning compile-time + runtime hybrid: Starlark + the native loader register at init; individual native libraries register dynamically.
- **Managed-OoP child binary.** Is the managed-OoP variant strictly the same binary as in-process mode, started with a different command-line flag (cleanest), or a separate companion binary? Cleanest is the same-binary approach.
- **Checkpoint payload size cap and large-payload handling.** What is the soft cap, and is there an out-of-row store (object store reference) for large payloads?
- **Tenant policy granularity.** Should deployment mode (in-process vs. managed-OoP) be declarable at the tenant level, the callable level, or both? Likely both (callable opts in; tenant policy can force).

## 14. Traceability

- **Parent module PRD:** [../../../docs/PRD.md](../../../docs/PRD.md)
- **Parent module DESIGN:** [../../../docs/DESIGN.md](../../../docs/DESIGN.md)
- **Parent decision (thin host model):** [serverless-runtime ADR-0005](../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)
- **Architecture decision realized by this plugin:** [ADR-0001](ADR/0001-cpt-cf-composed-runtime-plugin-adr-composed-runtime.md)
- **Embedded executors hosted by this plugin:**
  - Starlark — [ADR-0002](ADR/0002-cpt-cf-composed-runtime-plugin-adr-starlark-runtime.md)
  - Native Rust — [ADR-0003](ADR/0003-cpt-cf-composed-runtime-plugin-adr-native-rust-executor.md)
- **Future ADRs that build on this plugin:** Security model (NEXT_ADR_SCOPE.md ADR-6), Runtime Capabilities SDK (ADR-7), Debugging and Observability (ADR-8), Advanced Workflow Patterns (ADR-9). See [../../../docs/NEXT_ADR_SCOPE.md](../../../docs/NEXT_ADR_SCOPE.md).
