---
status: accepted
date: 2026-04-29
---
<!--
 =============================================================================
 ARCHITECTURE DECISION RECORD (ADR) — based on MADR format
 =============================================================================
 PURPOSE: Capture WHY the Composed Runtime plugin (one of the runtime plugins
 under the thin-host model in serverless-runtime ADR-0005) is structured as a shared, managed
 environment for embedded language executors — with a GTS-keyed callable
 router, a unified ExecutionContext (checkpointing, eventing, replay, sync +
 async invocation), and location transparency over ModKit's existing OoP gRPC
 abstraction for the managed-out-of-process variant.

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
# ADR — Composed Runtime Plugin: Shared Environment for Embedded Executors


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option A: Per-executor invocation paths (no internal router)](#option-a-per-executor-invocation-paths-no-internal-router)
  - [Option B: Always split-process (every embedded executor over gRPC)](#option-b-always-split-process-every-embedded-executor-over-grpc)
  - [Option C: Always single-process (no managed-OoP escape hatch)](#option-c-always-single-process-no-managed-oop-escape-hatch)
  - [Option D: Composed Runtime plugin with mode-transparent dispatch — chosen](#option-d-composed-runtime-plugin-with-mode-transparent-dispatch--chosen)
- [More Information](#more-information)
  - [Goals](#goals)
  - [Non-goals](#non-goals)
  - [Architectural Sketch](#architectural-sketch)
  - [SDK Contract Surface (`serverless-runtime-sdk`)](#sdk-contract-surface-serverless-runtime-sdk)
  - [Reuse of ModKit Primitives](#reuse-of-modkit-primitives)
  - [Unified ExecutionContext (sketch)](#unified-executioncontext-sketch)
  - [Embedded-Executor Interface (Forward Extensibility)](#embedded-executor-interface-forward-extensibility)
  - [Composition Examples](#composition-examples)
  - [Mode Selection](#mode-selection)
  - [Debug and Trace](#debug-and-trace)
  - [Relationship to Other ADRs](#relationship-to-other-adrs)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-composed-runtime-plugin-adr-composed-runtime`

## Context and Problem Statement

The thin-host decision in [serverless-runtime ADR-0005](../../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md) places invocation, scheduling, retry, compensation, checkpointing, and event-trigger handling inside runtime plugins; the host owns only Registry, Tenant Policy, REST/JSON-RPC/MCP surfaces, GTS validation, audit aggregation, plugin dispatch, and a lightweight invocation index. The host↔plugin contract is the `RuntimeAdapter` / `ServerlessRuntimeClient` / `FunctionHandler<I, O>` / `WorkflowHandler<I, O>` / `Context` / `Environment` / `ServerlessSdkError` surface in [`serverless-runtime-sdk`](../../../../serverless-sdk/docs/DESIGN.md). This ADR specifies **the Composed Runtime plugin** — one of the runtime plugins implementing `RuntimeAdapter` — and is the focus runtime for the platform.

The Composed Runtime plugin is the home for code-level callables that benefit from running inside the platform process (low latency, no IPC tax, shared trace plane) and for the **managed out-of-process** variant of the same artifacts (the same plugin binary launched as a child process for isolation). Deep integration with external runtimes — Temporal ([serverless-runtime ADR-0003](../../../../docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md), [serverless-runtime ADR-0004](../../../../docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md)), cloud FaaS bridges (Lambda, Cloud Functions, Azure Functions), or third-party orchestrators — is **out of scope** for this plugin; those backends, when needed, are separate fat plugins under the same thin-host model and are not composed inside this environment.

Inside the Composed Runtime plugin, the platform must host **multiple embedded executors** — Starlark today ([ADR-0002](0002-cpt-cf-composed-runtime-plugin-adr-starlark-runtime.md)), native Rust today ([ADR-0003](0003-cpt-cf-composed-runtime-plugin-adr-native-rust-executor.md)), and future embedded languages (CEL, additional scripting / DSL options) — and let authors compose freely across them. A Starlark function can invoke a native Rust callable; a native Rust callable can invoke a Starlark function; both run under one trace and one durability story. Two problems flow from this:

1. **Routing.** Embedded-callable invocations need a single dispatch surface keyed by GTS ID with generic JSON / typed-struct payloads, so caller code does not change shape based on which embedded executor implements the callee. The dispatch must transparently target an in-process executor or the managed-out-of-process instance of the same plugin (running in a child process for isolation), depending on deployment policy.
2. **Unified state and eventing.** Each embedded executor has its own natural checkpoint format — a Starlark suspended-frame snapshot, a native function's intermediate state struct, a future CEL expression's evaluation cursor. The plugin must give every embedded callable a generic API to detect replay mode, look up its prior checkpoint, persist a new one, wait on external events, and emit progress — while letting each executor own the schema of its checkpoint payload. Without this, replay correctness, memoization, event-trigger fan-in, and trace reconstruction fragment per executor.

Callables hosted here cover the full operational shape required by the platform: API-driven request/response functions (sync), event-driven functions (async, triggered by event broker delivery or schedule firing), and stateful workflows (long-running, suspend/resume, durable checkpointing). All three shapes share one router, one `ExecutionContext`, one checkpoint store, and one trace plane inside this plugin. Each registered callable is surfaced to the host as a `FunctionHandler<I, O>` or `WorkflowHandler<I, O>` SDK impl built by this plugin; that handler wraps the authoring asset (Starlark source, native `cdylib` entry, future CEL expression) and delegates execution to the embedded executor through the in-plugin router.

ModKit already provides the building blocks for the managed-OoP half: `ClientHub` (`libs/modkit/src/client_hub.rs`) is a type-safe interface registry with GTS-scoped lookup (`ClientScope::gts_id`); the OoP backend (`libs/modkit/src/bootstrap/oop.rs`, `libs/modkit/src/backends/`) spawns and supervises out-of-process modules; `modkit-transport-grpc` provides the gRPC client transport; `DirectoryService` handles module registration and heartbeat. This plugin composes these rather than invent a parallel IPC layer.

## Decision Drivers

* **Subordinate to serverless-runtime ADR-0005, contract anchored in `serverless-runtime-sdk`:** this plugin is one runtime plugin under the thin-host model; it implements `RuntimeAdapter` from the SDK and consumes `ServerlessRuntimeClient` for index/timeline events; it owns its own invocation engine, scheduler, event-trigger handling, retry, compensation, and checkpoint store, and surfaces only index/timeline events back to the host via the SDK event port
* **Embedded-executor extensibility:** Starlark, native Rust, and future embedded languages (CEL etc.) must plug in through one internal `EmbeddedExecutor` interface without runtime-core or caller-side changes
* **Composability across embedded executors:** any embedded callable must be able to invoke any other embedded callable by GTS ID, regardless of executor, with one signature
* **Managed in-process / out-of-process transparency:** caller code must not know whether the callee runs in-process or in a managed child process; deployment policy chooses per callable for isolation, resource enforcement, or hot-reload purposes
* **Local-dev simplicity:** a single binary must be able to host the plugin with all embedded executors and no IPC overhead
* **Reuse of ModKit primitives:** `ClientHub`, OoP backends, `modkit-transport-grpc`, and `DirectoryService` are reused for the managed-OoP variant; no parallel IPC stack
* **Unified `ExecutionContext`:** every embedded executor uses a single replay-aware execution-context API — checkpoint exists / current attempt / write checkpoint / read prior checkpoint / wait on event / emit progress — even though each executor owns its own checkpoint schema
* **Per-callable checkpoint schema ownership:** the plugin memoizes opaque checkpoint payloads, each tagged with a GTS schema ID owned by the executor; the plugin does not interpret the payload bytes
* **Sync and async invocation in one environment:** request/response functions, event-triggered functions, scheduled functions, and stateful workflows share the same router, ExecutionContext, and checkpoint store inside this plugin
* **Single trace/debug plane inside the plugin:** tracing, distributed-trace propagation, and step-level debug events flow through the router so observability is uniform across embedded executors and across the in-process / managed-OoP boundary
* **Multi-tenant isolation:** the router applies the same `tenant_id` / `user_id` scoping to every dispatch, regardless of executor
* **Out of scope for deep integration:** Temporal, cloud FaaS bridges, and other external orchestrators are separate fat plugins under serverless-runtime ADR-0005 and are not composed inside this plugin's router

## Considered Options

* **Option A**: Per-executor invocation paths (no internal router) — each embedded executor exposes its own invocation API and authors glue manually
* **Option B**: Always split-process (every embedded executor lives in its own child process reached over gRPC)
* **Option C**: Always single-process (all embedded executors statically linked, no managed-OoP escape hatch)
* **Option D**: Composed Runtime plugin — single `RuntimeAdapter` impl hosting multiple embedded executors behind an internal GTS-keyed router, with a managed-OoP variant over ModKit's existing `ClientHub` + OoP abstraction, mode chosen per callable — **chosen**

## Decision Outcome

Chosen option: **"Option D: Composed Runtime plugin with mode-transparent dispatch over ModKit's `ClientHub` + OoP abstraction"**.

The Composed Runtime plugin is implemented as a ModKit module that implements the `RuntimeAdapter` trait from [`serverless-runtime-sdk`](../../../../serverless-sdk/docs/DESIGN.md) (the plugin contract introduced by [serverless-runtime ADR-0005](../../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)). It owns its own callable registry, router, scheduler, event-trigger handling, retry/compensation engine, and durable checkpoint store, and emits index/timeline events back to the host through `ServerlessRuntimeClient`. Externally, the host treats it like any other fat plugin; internally, it provides the shared environment for embedded executors described below.

Every embedded executor — the Starlark executor in ADR-0002, the native Rust executor in ADR-0003, and any future embedded executor (CEL, additional scripting languages) — implements a small `EmbeddedExecutor` trait (`id`, `checkpoint_schema_id`, `invoke` — see the [trait sketch](#embedded-executor-interface-forward-extensibility)) and registers itself with the plugin at module start. In-process executors live in an **executor-keyed** registry (`executor_id → Arc<dyn EmbeddedExecutor>`); a callable configured for managed-OoP is **additionally** registered into `ClientHub` under the `dyn EmbeddedExecutor` trait and a `ClientScope::gts_id(callable_id)` scope. The two registrations back the two dispatch paths described below — per-callable `ClientHub` scoping is the routing key for the out-of-process path only, not the universal resolution model.

In-plugin code dispatches by GTS ID with a generic request (JSON, or a typed struct that serializes to it) and gets a generic response back. The router resolves `callable_id → (executor_id, mode)` from the Callable Registry, then takes one of two paths:

* **In-process (default):** look up `Arc<dyn EmbeddedExecutor>` directly in the Embedded Executor Registry by `executor_id` and call it — a function call plus an `Arc` clone, with no `ClientHub` lookup and no IPC. This is the hot path; keeping it free of per-callable scoping and IPC is exactly why in-process is the default.
* **Managed-OoP:** resolve the executor **per callable** via `ClientHub::get_scoped::<dyn EmbeddedExecutor>(ClientScope::gts_id(callable_id))`, which yields a `modkit-transport-grpc` client to the child-process instance hosting that callable. Per-callable scoping is required here precisely because each managed-OoP callable needs its own gRPC client bound to the right child process.

Whether the resolved executor is a function pointer in this process or a `modkit-transport-grpc` client targeting a managed child process is invisible at the call site. Deployment policy chooses per callable: trusted callables and dev-mode default to in-process; third-party / untrusted / resource-greedy callables can be marked managed-OoP and the plugin spawns a child process running the same plugin binary via the existing OoP backend (`LocalProcess` for self-managed, `K8s` for cluster, `Static` for pre-provisioned).

A unified `ExecutionContext` is passed to every embedded callable. It exposes:

* a replay-aware checkpoint/replay API — `is_replay()`, `attempt()`, `read_checkpoint(label) -> Option<CheckpointEnvelope>`, `write_checkpoint(label, schema_id, payload)` — backed by a single durable state envelope per invocation;
* an eventing API — `wait_event(filter) -> Future<Event>`, `emit_progress(payload)` — so async / event-triggered callables can suspend on external delivery and stateful workflows can fan out / fan in;
* a sync+async invocation API — `invoke(callable_id, request) -> Future<response>` — that dispatches through the in-plugin router regardless of whether the caller is a request/response function or a long-running workflow;
* tenant, security-context, trace, and correlation surfaces — propagated to every embedded executor by the plugin, not re-implemented per executor.

Each embedded executor declares a GTS schema ID for its checkpoint payload; the plugin stores the bytes opaquely, indexes by `(invocation_id, label, attempt)`, and surfaces the latest-attempt envelope back on resume. Memoization, trace, and debug events ride the same envelope: every executor write produces a trace event, every dispatch through the router emits a span, the debug plane subscribes to that stream, and the host's invocation index is updated through the SDK event port.

### Consequences

* Good, because authors use one signature for every embedded callable regardless of executor or in-process / managed-OoP placement
* Good, because the plugin composes existing ModKit primitives (`ClientHub`, OoP backends, `modkit-transport-grpc`, `DirectoryService`) instead of building a parallel IPC stack
* Good, because the in-process default keeps local dev and simple deployments single-binary, with zero IPC overhead
* Good, because the managed-OoP variant unlocks isolation for untrusted code and OS-level resource enforcement without caller changes
* Good, because per-callable checkpoint schemas let embedded executors evolve independently while the plugin guarantees uniform replay semantics
* Good, because trace/debug data has a single shape, gathered at the in-plugin router, regardless of which embedded executor produced it
* Good, because multi-tenant scoping and security-context propagation are enforced once, at the router, rather than re-implemented per executor
* Good, because adding a new embedded executor (CEL, future scripting) is a registration concern, not a plugin-core change
* Good, because the same `ExecutionContext` serves request/response, event-triggered, and stateful-workflow shapes — sync and async invocation share one durability story
* Good, because the plugin remains an internal abstraction beneath `RuntimeAdapter` — the host sees only standard plugin behavior and `ServerlessRuntimeClient` notifications
* Bad, because the in-plugin router becomes a hot-path component whose performance and correctness are critical (mitigated by the in-process default, which is a function call + `Arc` clone, no IPC)
* Bad, because per-callable JSON schemas must be maintained for every callable that crosses a managed-OoP boundary (mitigated by reusing GTS as the schema authority — schemas are already first-class on this platform)
* Bad, because embedded executors must respect the unified `ExecutionContext` even when their language model is richer (each executor decides what to expose as checkpoint envelopes and event-wait points)
* Bad, because in-process and managed-OoP dispatch have different failure modes (panic vs. broken pipe / child exit) that the router must normalize into a single error envelope surfaced through the SDK error taxonomy

### Confirmation

* Functional test: a Starlark callable invokes a native Rust callable which waits on an event and resumes. The chain succeeds with one trace, one tenant context, and one set of checkpoint envelopes attributable per executor; the host's invocation index reflects each transition.
* Mode-transparency test: the same callable is exercised once configured in-process and once configured managed-OoP. Caller code is unchanged. End-to-end behavior is identical aside from latency.
* Isolation test: a managed-OoP Starlark callable is induced to crash. The host process and all other in-flight invocations survive; the OoP backend restarts the child; the resumed callable replays from its last checkpoint envelope.
* Replay test: a stateful Starlark workflow and a stateful native-Rust callable both resume from a prior invocation. Each consults `is_replay()` and recovers via its own checkpoint schema; the plugin exposes a uniform timeline across both.
* Eventing test: an event-triggered callable suspends on `wait_event`; external event delivery resumes it; `attempt()` and `is_replay()` reflect the resumption; the response is emitted normally.
* Sync vs. async test: a request/response function and an async event-triggered function are dispatched through the same router; both produce timeline events through the SDK event port; the host index surfaces both consistently.
* Trace test: end-to-end trace shows one span tree across embedded executors and across the in-process / managed-OoP boundary; OpenTelemetry context propagates over gRPC for managed-OoP hops.
* Extensibility test: a stub CEL executor is added by registering one `EmbeddedExecutor` implementation; no plugin-core changes are required; callables expressed in CEL are dispatched, checkpointed, and traced under the same `ExecutionContext`.
* Performance test: in-process router dispatch p95 < 10µs (function call + lock-free read); managed-OoP dispatch p95 dominated by gRPC RTT.

## Pros and Cons of the Options

### Option A: Per-executor invocation paths (no internal router)

* Good, because each embedded executor stays simple and exposes its native API
* Bad, because composability degenerates: every cross-executor call needs custom glue
* Bad, because tracing, debug, replay, eventing, and tenant scoping fragment per executor
* Bad, because callers must know which executor implements which callable
* Bad, because adding a new embedded executor requires changing every cross-executor caller
* Bad, because async / event-triggered shapes have no shared suspend/resume surface and must re-implement eventing per executor

### Option B: Always split-process (every embedded executor over gRPC)

* Good, because isolation is uniform and strong
* Good, because the IPC abstraction is exercised everywhere, no bimodal behavior to test
* Bad, because local dev requires running multiple child processes for the simplest scenario
* Bad, because gRPC RTT dominates latency budgets for trivial calls (e.g. a native Rust function chained inside a Starlark loop)
* Bad, because operating multi-process by default fights the "ships as one binary for simple cases" requirement of the thin-host parent decision

### Option C: Always single-process (no managed-OoP escape hatch)

* Good, because the simplest implementation; one process, one address space, one trace
* Good, because lowest dispatch latency
* Bad, because there is no isolation story for untrusted code paths (third-party Starlark, customer-supplied native libraries)
* Bad, because a panic in any embedded executor takes down the host process and every other in-flight invocation
* Bad, because rebuild/redeploy is required to refresh customer-supplied code that should be hot-reloadable across a process boundary

### Option D: Composed Runtime plugin with mode-transparent dispatch — chosen

* Good, because in-process is the default and matches simple-case / local-dev expectations
* Good, because managed-OoP is opt-in per callable, used precisely where isolation or resource enforcement pays for the IPC tax
* Good, because the IPC layer is ModKit's existing `ClientHub` + OoP backend + `modkit-transport-grpc`, not a new system
* Good, because the router gives a single point to enforce tracing, replay, eventing, and tenant scoping for all embedded executors
* Good, because it composes naturally with the embedded executors specified in [ADR-0002](0002-cpt-cf-composed-runtime-plugin-adr-starlark-runtime.md) (Starlark) and [ADR-0003](0003-cpt-cf-composed-runtime-plugin-adr-native-rust-executor.md) (native Rust), and accepts future embedded executors with no plugin-core change
* Good, because the same `ExecutionContext` covers API-driven, event-oriented, sync, and async callables — and the stateful-workflow shape on top
* Good, because the plugin remains an internal abstraction beneath `RuntimeAdapter`
* Bad, because two dispatch paths (in-process and managed-OoP) must be tested and have their failure semantics normalized
* Bad, because per-callable schemas and the unified `ExecutionContext` impose a discipline executors must obey even when their native model is richer

## More Information

### Goals

- Position this plugin as **the focus `RuntimeAdapter` implementation** under the thin-host model: code-level, embedded-language callables with one shared environment.
- Provide one invocation surface (`Router::invoke(callable_id, request, ctx) -> response`) for every embedded callable inside the plugin.
- Provide a unified `ExecutionContext` with replay-aware checkpoint primitives, an eventing API, and sync+async invocation that every embedded executor uses.
- Cover the full operational shape of callables hosted here — API-driven (sync), event-triggered (async), scheduled, and stateful workflows — with one router, one checkpoint store, one trace plane.
- Provide a stable `EmbeddedExecutor` interface so additional embedded executors (CEL, future scripting) plug in without plugin-core changes and inherit checkpointing, eventing, and routing for free.
- Keep deployment policy (in-process vs. managed-OoP) declarative per callable; never leak it to caller code.
- Reuse ModKit's `ClientHub`, OoP backend, and `modkit-transport-grpc` for the managed-OoP variant.
- Emit clean invocation index updates back to the host via `ServerlessRuntimeClient`; remain a black box to the host beyond that.

### Non-goals

- Deep integration with external orchestrators — Temporal ([serverless-runtime ADR-0003](../../../../docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md), [serverless-runtime ADR-0004](../../../../docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md)), cloud FaaS bridges (Lambda, Cloud Functions, Azure Functions), or third-party engines. These are separate fat plugins under [serverless-runtime ADR-0005](../../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md) and are out of scope here.
- Defining executor-specific contracts (those live in [ADR-0002](0002-cpt-cf-composed-runtime-plugin-adr-starlark-runtime.md) and [ADR-0003](0003-cpt-cf-composed-runtime-plugin-adr-native-rust-executor.md)).
- Specifying the wire schemas for individual callables (each callable owns its `params`/`returns` per the GTS model in [serverless-runtime ADR-0001](../../../../docs/ADR/0001-cpt-cf-serverless-runtime-adr-callable-type-hierarchy.md)).
- Specifying the debug-API surface (deferred — see `NEXT_ADR_SCOPE.md`).
- Picking between TCP gRPC and named-pipe gRPC for managed-OoP transport (handled by `modkit-transport-grpc` configuration).
- **Cross-plugin `ctx.invoke`.** The in-plugin router resolves only callables registered with this plugin. A callee hosted in a *different* plugin (e.g. a DSL/Temporal callable) is **not reachable through `ctx.invoke`** — `ExecutionContext::invoke` raises a "callable not found" error for an unknown callable, and the SDK defines no plugin→host outbound-routing path for a plugin to ask the host to route an outbound call. Cross-plugin composition, when required, is arranged at the host orchestration layer, not from inside a callable. Specifying such a reverse path is future work owned by the SDK, not this ADR.

### Architectural Sketch

The plugin owns five collaborating components, all in-process to the plugin module:

1. **Callable Registry.** A map from callable GTS ID → `(embedded_executor_id, deployment_mode, checkpoint_schema_id, traits)`. Populated at plugin start from durable definitions and at dynamic registration time (e.g. user-uploaded Starlark, hot-loaded native libraries).
2. **Embedded Executor Registry.** A map from executor ID → `Arc<dyn EmbeddedExecutor>`. In-process embedded executors are constructed at plugin init. Managed-OoP variants are represented by a `modkit-transport-grpc` client to a sibling instance of this plugin running as a child process, registered under the same trait type via `ClientHub` so the router can resolve them uniformly.
3. **Router.** Resolves `callable_id → (executor_id, mode)` via the Callable Registry, then dispatches on `mode`: for **in-process** callables it indexes the Embedded Executor Registry directly by `executor_id` (a trait call, no `ClientHub`); for **managed-OoP** callables it resolves per callable via `ClientHub::get_scoped::<dyn EmbeddedExecutor>(ClientScope::gts_id(callable_id))` to obtain the `modkit-transport-grpc` client for the child process. Either way it issues the dispatch with a wrapped `ExecutionContext`, applies tenant scoping, and emits trace spans.
4. **State Envelope Store.** Backed by `modkit-db`. One row per `(invocation_id, label, attempt)`; columns include `schema_id`, `payload` (opaque bytes — JSON or executor-defined binary), `created_at`. `ExecutionContext::read_checkpoint(label)` returns the latest-attempt envelope; `write_checkpoint` inserts a row for the current attempt.
5. **Event Hub.** Plugin-local event-bus client that delivers external events (event-trigger firings, schedule firings) to suspended invocations via `ExecutionContext::wait_event`, and emits progress notifications to the host's invocation index via `ServerlessRuntimeClient`.

### SDK Contract Surface (`serverless-runtime-sdk`)

The Composed Runtime plugin is the platform's first `RuntimeAdapter` implementation. Two layers meet inside it:

| SDK contract (host-facing) | Composed Runtime internal surface | Purpose |
|----------------------------|-----------------------------------|---------|
| `RuntimeAdapter` (plugin impls, host calls) | `RuntimeAdapter` impl on the plugin module's top-level type | Invocation / control / schedule / event-trigger entrypoints; one impl per plugin |
| `ServerlessRuntimeClient` (host impls, plugin calls) | resolved through `ClientHub`; called by Event Hub and Invocation Engine | Index updates and timeline events flow back to the host through this client; the plugin never opens its own host RPC |
| `FunctionHandler<I, O>` / `WorkflowHandler<I, O>` (plugin impls, plugin calls) | one impl built per registered callable, wrapping the authoring asset; the plugin's `RuntimeAdapter::execute` resolves the callable via the in-plugin router and delegates to the handler | Each handler is a thin adapter that materializes the embedded executor's request shape, calls `EmbeddedExecutor::invoke`, and surfaces the result as the SDK's typed `O` / `ServerlessSdkError` |
| `Context` (read-only handler-author view of `InvocationRecord`) | constructed once per invocation by `RuntimeAdapter::execute` and embedded into the `ExecutionContext` accessor (`ctx.sdk_context()`) | Identity, deadlines, correlation; not mutable, not stateful — distinct from this plugin's richer `ExecutionContext` |
| `Environment` (sync env/secret resolution) | constructed by `RuntimeAdapter::execute` from the host-supplied credstore snapshot; exposed to embedded executors via `ExecutionContext::environment()` and reachable in-language (e.g. Starlark `ctx.env.get(name)`) | Pre-fetched secrets and config; never async |
| `ServerlessSdkError` / `RuntimeErrorCategory` / `RuntimeErrorPayload` | the plugin maps `EmbeddedExecutorError` to `ServerlessSdkError`; runtime resource-limit hits map to `RuntimeErrorCategory::RuntimeLimit`; replay/invariant violations map to `RuntimeErrorCategory::Internal` | One error envelope shape leaves the plugin; embedded executors do not see the SDK error type directly |
| `TimelineEventType` | emitted by the Invocation Engine and Event Hub through `ServerlessRuntimeClient` | One event stream per invocation; the host's lightweight index is built from these |

Two consequences worth stating explicitly:

1. **The SDK `Context` is a projection inside `ExecutionContext`, not a replacement for it.** Embedded executors receive `Arc<dyn ExecutionContext>` (defined below). `Context` (the SDK type) is one of the things `ExecutionContext` carries — read-only identity and deadline view. Checkpoint, event-wait, progress emission, and `invoke` live on `ExecutionContext` only; `Context` stays minimal as the SDK intends.
2. **No engine dependency leaks into the SDK.** The plugin depends on `serverless-runtime-sdk`; the SDK does not depend on the plugin or on any embedded executor. Embedded-executor crates depend on a separate internal plugin SDK (the `EmbeddedExecutor` trait and `ExecutionContext` trait), not on `serverless-runtime-sdk` directly.

### Reuse of ModKit Primitives

| Concern | ModKit primitive |
|---------|------------------|
| Type-safe **managed-OoP** executor resolution (per-callable gRPC client) | `ClientHub` (`libs/modkit/src/client_hub.rs`), keyed by `dyn EmbeddedExecutor` and `ClientScope::gts_id` — the routing key for the out-of-process path; in-process dispatch is a direct executor-keyed registry lookup that bypasses `ClientHub` |
| Managed child-process spawn / supervise | `OopBackend` and `ModuleRuntimeBackend` traits (`libs/modkit/src/backends/`), with `LocalProcessBackend`, K8s, and Static backends |
| Managed-OoP bootstrap / heartbeat / shutdown | `bootstrap::oop::run_oop_with_options` (`libs/modkit/src/bootstrap/oop.rs`) |
| Module discovery / registration | `DirectoryService` (`libs/modkit/src/directory.rs`) |
| gRPC transport for managed-OoP hops | `modkit-transport-grpc` (`libs/modkit-transport-grpc/`) |
| Persistent checkpoint store | `modkit-db` |
| Plugin-style registration of embedded executors | `libs/modkit/src/runtime/grpc_installers.rs` and the existing module bootstrap pattern |

The plugin adds, on top: the Callable Registry, the unified `ExecutionContext`, the `EmbeddedExecutor` trait, the State Envelope Store, and the Event Hub.

### Unified ExecutionContext (sketch)

```rust
pub trait ExecutionContext: Send + Sync {
    fn invocation_id(&self) -> InvocationId;
    fn callable_id(&self) -> &GtsId;
    fn tenant_id(&self) -> &TenantId;
    fn security_context(&self) -> &SecurityContext;
    fn trace(&self) -> &TraceContext;

    /// Read-only projection of the SDK's `Context` (identity, deadlines,
    /// correlation). Constructed once by the plugin's `RuntimeAdapter::execute`
    /// from the host-supplied `InvocationRecord`. Not mutable here.
    fn sdk_context(&self) -> &serverless_runtime_sdk::Context;

    /// Pre-populated SDK `Environment` (sync env/secret access). Resolved by
    /// the plugin via `CredStoreEnvironment` before dispatch.
    fn environment(&self) -> &dyn serverless_runtime_sdk::Environment;

    /// True when the embedded executor is replaying a previously-suspended invocation.
    fn is_replay(&self) -> bool;
    fn attempt(&self) -> u32;

    /// Look up a prior checkpoint for this invocation by label. The returned
    /// envelope carries the executor-owned schema_id and opaque payload.
    fn read_checkpoint(&self, label: &str) -> Option<CheckpointEnvelope>;

    /// Persist a new checkpoint. The plugin stores the payload opaquely under
    /// the executor-declared schema_id. Subsequent replays will see it through
    /// `read_checkpoint`.
    fn write_checkpoint(&self, label: &str, schema_id: &GtsId, payload: Bytes) -> Result<()>;

    /// Suspend the current invocation until an external event matching `filter`
    /// is delivered. On resumption, `is_replay()` is true and the returned event
    /// is also visible through the timeline event port.
    fn wait_event(&self, filter: EventFilter) -> BoxFuture<'_, Result<Event>>;

    /// Emit a progress notification (timeline event) to the host's invocation
    /// index via the SDK event port. Non-blocking; no waiting for ack.
    fn emit_progress(&self, payload: ProgressPayload) -> Result<()>;

    /// Invoke another callable through the in-plugin router. Honors deployment
    /// mode (in-process or managed-OoP). Used by both request/response callables
    /// and stateful workflows.
    fn invoke(&self, callee: &GtsId, request: Value) -> BoxFuture<'_, Result<Value>>;
}

pub struct CheckpointEnvelope {
    pub label: String,
    pub schema_id: GtsId,
    pub payload: Bytes,
    pub attempt: u32,
    pub created_at: DateTime<Utc>,
}
```

`EmbeddedExecutor::invoke(&self, ctx, request)` is the dispatch contract every embedded executor implements. The router constructs `ctx` from the callable record + invocation envelope; the executor decides what to checkpoint, when to wait on events, and what progress to emit.

### Embedded-Executor Interface (Forward Extensibility)

The `EmbeddedExecutor` trait is the single integration point for new embedded languages:

```rust
pub trait EmbeddedExecutor: Send + Sync {
    /// Identifier for the executor (e.g. "starlark", "native-rust", "cel").
    fn id(&self) -> &str;

    /// GTS schema ID for this executor's checkpoint envelope payloads.
    fn checkpoint_schema_id(&self) -> &GtsId;

    /// Dispatch one invocation. Receives the unified ExecutionContext and the
    /// request value; returns a response or surfaces a suspension via wait_event.
    fn invoke(&self, ctx: Arc<dyn ExecutionContext>, request: Value)
        -> BoxFuture<'_, Result<Value, EmbeddedExecutorError>>;
}
```

Adding a new embedded language (CEL, future scripting languages, additional DSLs) is one `impl EmbeddedExecutor` plus a registration call at plugin init — checkpointing, eventing, routing, tenant scoping, trace, and managed-OoP transparency come for free.

### Composition Examples

* **Starlark calling native.** A Starlark workflow's `ctx.invoke(callee_id, input)` calls the in-plugin router; the resolved embedded executor is the native Rust executor ([ADR-0003](0003-cpt-cf-composed-runtime-plugin-adr-native-rust-executor.md)). If marked managed-OoP for isolation, the call hits a `modkit-transport-grpc` client to a child-process instance of this plugin; if in-process, it's a direct trait call. In either case the native callable receives the same `ExecutionContext` and writes its own checkpoint envelope.
* **Native calling Starlark.** A native callable invokes a Starlark callable through `ctx.invoke(...)`. The native function does not know that the Starlark code is sandboxed in a separate child process for tenant isolation; the router transparently routes via `ClientHub`.
* **Event-triggered async function.** A Starlark function declared as event-triggered suspends on `ctx.wait_event({event_type: ...})` after partial work, persisting its frame via `ctx.write_checkpoint`. When the event arrives via the Event Hub, the invocation resumes from the checkpoint, completes, and emits a timeline event back to the host.
* **Stateful workflow.** A Starlark or native callable declared with workflow traits runs across multiple suspension/resumption cycles, fanning out child invocations via `ctx.invoke`, waiting on event correlation via `ctx.wait_event`, and persisting state at every safe point. The host index shows one invocation with a full timeline of step events.

### Mode Selection

Deployment mode (`in_process` | `managed_oop_local_process` | `managed_oop_k8s` | `managed_oop_static`) is per callable, declared in the callable's traits or in tenant policy:

* Default is `in_process`.
* `managed_oop_*` modes are used for: third-party / untrusted code (e.g. customer-supplied Starlark), heavy native dependencies the host should not link, callables marked for resource-quota enforcement at the OS-process level, and fault-isolation (a callable that occasionally crashes).

The plugin materializes mode at registration time. Changing a callable's mode requires re-registration (and, for managed-OoP modes, restarting the corresponding child-process instance of this plugin).

### Debug and Trace

* The in-plugin router emits one span per dispatch, regardless of embedded executor or in-process / managed-OoP boundary. OpenTelemetry context propagates over `modkit-transport-grpc` for managed-OoP hops.
* `ExecutionContext::write_checkpoint`, `wait_event`, and every `Router::invoke` produce trace events on a per-invocation timeline.
* Timeline events are forwarded to the host's invocation index via `ServerlessRuntimeClient`; the host never reads checkpoint payloads directly.
* The future debug ADR (see `NEXT_ADR_SCOPE.md`) layers breakpoints / step-through on top of these primitives — the debugger is a router subscriber inside this plugin, not an executor-specific concern.

### Relationship to Other ADRs

- **[serverless-runtime ADR-0001](../../../../docs/ADR/0001-cpt-cf-serverless-runtime-adr-callable-type-hierarchy.md):** Function and Workflow are sibling base types. The in-plugin router treats both uniformly; `workflow_traits` direct the plugin to expect long-running / suspendable behavior and to provision durable checkpoint storage.
- **[serverless-runtime ADR-0002](../../../../docs/ADR/0002-cpt-cf-serverless-runtime-adr-jsonrpc-mcp-protocol-surfaces-v1.md):** Protocol surfaces sit at the host. The host's plugin-dispatch routes the requests to this plugin via `RuntimeAdapter`; this plugin then dispatches to its embedded executors through the internal router.
- **[serverless-runtime ADR-0003](../../../../docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md), [serverless-runtime ADR-0004](../../../../docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md):** A separate fat plugin (DSL-on-Temporal) is the home for declarative workflow callables. That plugin is a peer of this one under [serverless-runtime ADR-0005](../../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md). `ctx.invoke` does **not** reach callables in that plugin — the in-plugin router resolves only callables registered with this plugin (see Non-goals). Composition between this plugin's callables and DSL/Temporal callables, if and when needed, is arranged at the host orchestration layer; the SDK exposes no plugin→host outbound-routing path today, so it is not something a callable can reach through `ctx.invoke`.
- **[serverless-runtime ADR-0005](../../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md):** Parent decision. This plugin implements `RuntimeAdapter` from `serverless-runtime-sdk` (see the [SDK PRD](../../../../serverless-sdk/docs/PRD.md) and [SDK DESIGN](../../../../serverless-sdk/docs/DESIGN.md)), owns its own engine/scheduler/triggers, and emits index/timeline events through `ServerlessRuntimeClient`.
- **[ADR-0002](0002-cpt-cf-composed-runtime-plugin-adr-starlark-runtime.md):** First embedded executor inside this plugin (code-as-orchestration). Implements `EmbeddedExecutor` and uses `ExecutionContext` for checkpoints, events, and cross-callable invocation.
- **[ADR-0003](0003-cpt-cf-composed-runtime-plugin-adr-native-rust-executor.md):** Second embedded executor inside this plugin (hot-loadable native Rust). Implements `EmbeddedExecutor`; hot reload rides this plugin's checkpoint store.

## Traceability

- **Plugin PRD**: [../PRD.md](../PRD.md)
- **Plugin DESIGN**: [../DESIGN.md](../DESIGN.md)
- **Parent gear PRD**: [../../../../docs/PRD.md](../../../../docs/PRD.md)
- **Parent gear DESIGN**: [../../../../docs/DESIGN.md](../../../../docs/DESIGN.md)

This decision directly addresses the following requirements and design elements:

* `cpt-cf-serverless-runtime-principle-pluggable-adapters` — the internal `EmbeddedExecutor` interface is the integration point for embedded-language extensibility within this plugin
* `cpt-cf-serverless-runtime-principle-impl-agnostic` — caller code is decoupled from embedded-executor identity and from in-process / managed-OoP placement
* `cpt-cf-serverless-runtime-component-executor` — each embedded executor is one implementation of `EmbeddedExecutor`; the in-plugin router resolves the right one
* `cpt-cf-serverless-runtime-fr-execution-engine` — durable state and replay are plugin-owned, executor-schema-extensible
* `cpt-cf-serverless-runtime-fr-execution-lifecycle` — uniform invocation, suspend, resume, cancel via the in-plugin router and `ExecutionContext`
* `cpt-cf-serverless-runtime-fr-runtime-capabilities` — runtime capabilities (HTTP, events, audit) are exposed through `ExecutionContext` regardless of embedded executor
* `cpt-cf-serverless-runtime-fr-trigger-schedule` — schedule and event-trigger firings dispatch through the in-plugin router and surface as invocations to embedded executors
* `cpt-cf-serverless-runtime-nfr-security` — tenant-scoping and security-context propagation enforced once, at the in-plugin router
* `cpt-cf-serverless-runtime-nfr-reliability` — unified checkpoint envelopes back replay correctness across embedded executors
* `cpt-cf-serverless-runtime-nfr-tenant-isolation` — managed-OoP mode provides OS-level isolation for untrusted callables without changing caller code
* `cpt-cf-serverless-runtime-nfr-ops-traceability` — single trace plane spans embedded-executor boundaries and the in-process / managed-OoP boundary via `modkit-transport-grpc` context propagation
