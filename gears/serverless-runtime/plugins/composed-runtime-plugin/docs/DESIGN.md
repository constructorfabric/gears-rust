# Technical Design — Composed Runtime Plugin


<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
  - [1.4 Component Boundaries: Host vs. This Plugin](#14-component-boundaries-host-vs-this-plugin)
  - [1.5 ModKit Integration](#15-modkit-integration)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Mode Resolution and Managed-OoP Dispatch](#34-mode-resolution-and-managed-oop-dispatch)
  - [3.5 Interactions & Sequences](#35-interactions--sequences)
  - [3.6 Database schemas & tables](#36-database-schemas--tables)
  - [3.7 Hot Reload (for embedded executors that support it)](#37-hot-reload-for-embedded-executors-that-support-it)
  - [3.8 Observability](#38-observability)
  - [3.9 Errors](#39-errors)
  - [3.10 Reuse of ModKit Primitives](#310-reuse-of-modkit-primitives)
- [4. Additional Context](#4-additional-context)
  - [4.1 Out of Scope (and Why)](#41-out-of-scope-and-why)
  - [4.2 Non-Applicable Domains](#42-non-applicable-domains)
- [5. Traceability](#5-traceability)

<!-- /toc -->

**Module:** `cyberware-composed-runtime-plugin`
**Parent module:** `cyberware-serverless-runtime` (host)
**ID prefix:** cpt-cf-composed-runtime-plugin-{kind}-{slug}

## 1. Architecture Overview

### 1.1 Architectural Vision

The Composed Runtime plugin is one of the runtime plugins delivered under the Serverless Runtime thin-host model ([ADR-0005](../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)). It is the focus runtime for code-level callables on the platform: a single plugin that hosts multiple **embedded language executors** (Starlark, native Rust, future CEL, future Wasm) on a shared in-plugin environment, so that callables expressed in different embedded languages compose freely through one `ExecutionContext` and inherit durability, replay, eventing, tracing, tenant scoping, and a path to isolation for free.

The plugin satisfies the `RuntimeAdapter` trait defined in `serverless-runtime-sdk`; externally, the host treats it like any other fat plugin and never depends on it at compile time. Internally, the plugin owns its own invocation engine, scheduler, event-trigger handling, retry, compensation, checkpoint store, and embedded-executor registry — every concern the thin-host model places inside runtime plugins ([ADR-0005](../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)).

The plugin is delivered in two operational modes from one binary:

- **In-process** (default) — the plugin runs as a ModKit module inside the host process; dispatch through the in-plugin router is a direct function call.
- **Managed-out-of-process** (per-callable opt-in) — the same plugin binary runs as a child process supervised by ModKit's existing OoP backend (`LocalProcessBackend` / `K8sBackend` / `StaticBackend`); the host-process plugin instance routes selected callables to the child via `modkit-transport-grpc` resolved through `ClientHub`. Caller code is unchanged across modes.

Deep integration with external orchestrators (Temporal, cloud FaaS bridges, third-party engines) is **out of scope** here and is delivered, when needed, as separate fat plugins under the same thin-host model.

### 1.2 Architecture Drivers

| Requirement | Design Response |
|-------------|-----------------|
| `cpt-cf-composed-runtime-plugin-fr-embedded-executor-extensibility` (FR-001) | A single internal `EmbeddedExecutor` trait; embedded-executor crates depend on a small internal SDK and register at plugin init; no plugin-core change required to add a new language |
| `cpt-cf-composed-runtime-plugin-fr-in-plugin-router` (FR-002) | GTS-keyed Callable Registry → Embedded Executor Registry; lookup via `ClientHub::get_scoped::<dyn EmbeddedExecutor>(ClientScope::gts_id(callable_id))` |
| `cpt-cf-composed-runtime-plugin-fr-execution-context` (FR-003) | Unified `ExecutionContext` trait passed to every dispatch; checkpoint, event-wait, progress-emit, sync+async invocation, identity, trace surfaces |
| `cpt-cf-composed-runtime-plugin-fr-checkpoint-store` (FR-004) | Durable `(invocation_id, label, schema_id, payload, attempt, created_at)` table in `modkit-db`; payload opaque to the plugin |
| `cpt-cf-composed-runtime-plugin-fr-event-hub` (FR-005) | Plugin-local Event Hub subscribes to the platform event broker; matches deliveries to `wait_event` subscriptions; resumes invocations from their checkpoint envelopes |
| `cpt-cf-composed-runtime-plugin-fr-operational-shapes` (FR-006) | One router + one `ExecutionContext` covers sync request/response, event-triggered async, scheduled, and stateful workflow shapes |
| `cpt-cf-composed-runtime-plugin-fr-deployment-modes` (FR-007) | Per-callable mode flag; in-process binds the executor directly; managed-OoP modes use ModKit OoP backends to spawn a child instance of this plugin and route over `modkit-transport-grpc` |
| `cpt-cf-composed-runtime-plugin-fr-sdk-plugin-trait` (FR-008) | Implements `serverless-runtime-sdk`'s plugin trait; emits timeline events through the `ServerlessRuntimeClient` |
| `cpt-cf-composed-runtime-plugin-fr-retry-compensation-timeouts` (FR-009) | Plugin-internal retry engine, two-layer compensation (function-level and embedded-step-level), wall-clock and resource-limit enforcement |
| `cpt-cf-composed-runtime-plugin-fr-tenant-scoping` (FR-010) | Tenant / security context enforced at the router; embedded executors read it from `ExecutionContext` |
| `cpt-cf-composed-runtime-plugin-fr-trace-debug-plane` (FR-011) | One OpenTelemetry span per dispatch; per-invocation timeline event stream; future debug ADR subscribes here |
| `cpt-cf-composed-runtime-plugin-nfr-dispatch-performance` (NFR-001) | In-process dispatch is a lock-free read + `Arc` clone; no allocation on the hot path |
| `cpt-cf-composed-runtime-plugin-nfr-replay-correctness` (NFR-003) | Per-`(invocation_id, label)` durable envelopes; schema IDs are part of the envelope so resume validates compatibility |
| `cpt-cf-composed-runtime-plugin-nfr-tenant-isolation` (NFR-004) | Tenant-scoped queries on registry + checkpoint store; managed-OoP variant provides OS-level isolation for untrusted callables |
| `cpt-cf-composed-runtime-plugin-nfr-hot-reload` (NFR-005) | Drain-load-resume protocol on native libraries; no library-resident state; reload built on top of the checkpoint store |
| `cpt-cf-composed-runtime-plugin-nfr-modkit-reuse` (NFR-007) | No parallel IPC / supervision / RPC; managed-OoP rides ModKit's existing primitives |

### 1.3 Architecture Layers

The plugin is organized into four layers, all in-process to the plugin module (the same layers also run inside the managed-OoP child instance when configured). Layer responsibilities mirror the canonical ModKit DDD-light layout from `docs/modkit_unified_system/02_module_layout_and_sdk_pattern.md`.

| Layer | Responsibility | Technology |
|-------|----------------|------------|
| **SDK contract (inbound)** | Implements the `RuntimeAdapter` trait from `serverless-runtime-sdk`. Receives invocation, schedule, event-trigger, lifecycle, and policy calls from the host's plugin-dispatch. Emits index/timeline events through the `ServerlessRuntimeClient`. | Rust async traits via `dyn`-resolution through `ClientHub` |
| **Domain (plugin core)** | Owns the in-plugin Router, Callable Registry, Embedded Executor Registry, Invocation Engine (state machine, retry, compensation, timeout), Event Hub (broker subscriptions, schedule firings), and the `ExecutionContext` factory. Vendor-neutral, embedded-executor-agnostic. | Rust |
| **Embedded executors (plug-in surface)** | One implementation of the internal `EmbeddedExecutor` trait per language (Starlark per [ADR-0007](../../../docs/ADR/0007-cpt-cf-serverless-runtime-adr-starlark-runtime.md), native Rust per [ADR-0008](../../../docs/ADR/0008-cpt-cf-serverless-runtime-adr-native-rust-executor.md), future CEL). Each owns its checkpoint schema; opaque to the rest of the plugin. | Rust + per-language interpreter / loader (`starlark-rust`, `libloading`, …) |
| **Infrastructure (outbound)** | Persistence (`modkit-db`), cross-process transport for managed-OoP (`modkit-transport-grpc`), child-process supervision (`OopBackend` family), module discovery (`DirectoryService`), event broker, and outbound API gateway — all reached as `ClientHub`-resolved interfaces. | ModKit primitives |

The Embedded executor layer is the **only** layer extensible by adding a new crate; the other three layers are stable per plugin version.

### 1.4 Component Boundaries: Host vs. This Plugin

| Concern | Host (`cyberware-serverless-runtime`) | This Plugin |
|---------|---------------------------------------|-------------|
| Function / workflow definition registry | **Owns** | Reads via SDK |
| Tenant policy | **Owns** | Reads via SDK |
| REST / JSON-RPC / MCP surfaces | **Owns** | — |
| GTS validation at registration | **Owns** | — |
| Audit aggregation | **Owns** | Emits source events |
| Lightweight invocation index (id, function_id, adapter, tenant, owner, status, timestamps, error_summary) | **Owns** (populated from plugin events) | Emits index updates |
| Plugin dispatch (host → plugin GTS ID) | **Owns** | Receives |
| Invocation engine (start, suspend, resume, cancel) | — | **Owns** |
| Scheduler (cron / interval firings) | — | **Owns** |
| Event-trigger handling (broker subscriptions, filters) | — | **Owns** |
| Retry, compensation, timeout enforcement | — | **Owns** |
| Checkpoint envelope store | — | **Owns** |
| Embedded-executor registry and dispatch | — | **Owns** |
| Cross-callable invocation (within this plugin) | — | **Owns** (in-plugin router) |
| Cross-plugin invocation (e.g. → DSL/Temporal plugin) | **Owns** (plugin-dispatch) | — |

### 1.5 ModKit Integration

The plugin is a ModKit module declared with `#[modkit::module]`. Conventionally:

- **Crate location:** `modules/serverless-runtime/plugins/composed-runtime-plugin/`
- **Crate name:** `cyberware-composed-runtime-plugin`
- **Library name:** `composed_runtime_plugin`
- **Module GTS type:** TBD by the parent SDK (`serverless-runtime-sdk`); this plugin registers itself under that GTS type so the host's plugin-dispatch can resolve it via `ClientHub`.

The plugin has **no compile-time dependency from the host** in either direction beyond the SDK crate, in line with [ADR-0005 §1.4.1](../../../docs/DESIGN.md#14-modkit-integration).

A small internal SDK crate (working name: `cyberware-composed-runtime-embedded-sdk`) is expected to host the `EmbeddedExecutor` trait, `ExecutionContext`, and `CheckpointEnvelope` types so that the embedded-executor crates (Starlark per [ADR-0007](../../../docs/ADR/0007-cpt-cf-serverless-runtime-adr-starlark-runtime.md), native Rust per [ADR-0008](../../../docs/ADR/0008-cpt-cf-serverless-runtime-adr-native-rust-executor.md)) can depend on it without pulling in the whole plugin. This is a follow-up scoping question (see PRD Open Questions).

## 2. Principles & Constraints

### 2.1 Design Principles

#### One Environment, Many Languages

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-principle-shared-environment`

All code-level callables — regardless of source language — share the same router, `ExecutionContext`, checkpoint store, and event hub. Adding a new embedded language is one `EmbeddedExecutor` implementation plus a registration call; checkpointing, eventing, routing, tracing, and tenant scoping come for free.

#### Embedded Executors Are Interchangeable in Shape

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-principle-uniform-executors`

Every embedded executor implements the same small trait (`EmbeddedExecutor`), declares one checkpoint schema, and receives one `ExecutionContext` shape. The plugin treats every executor uniformly; callers never branch on executor identity.

#### Caller Unaware of Placement

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-principle-placement-transparency`

In-process vs. managed-out-of-process is a deployment-time decision, never a code-time one. The in-plugin router resolves placement transparently through `ClientHub`; caller code and request/response shapes are identical across modes.

#### Plugin Owns Its State

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-principle-plugin-owned-state`

Embedded callables hold no static / global / cross-invocation state of their own; the plugin's `ExecutionContext` and managed services own it. Cross-invocation persistence is checkpoints; cross-process shared resources (pools, clients) are resolved through `ClientHub`-resolved interfaces.

#### Reuse ModKit Primitives

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-principle-modkit-reuse`

No parallel IPC, supervision, RPC, persistence, or directory mechanism is introduced. Managed-OoP rides ModKit's existing `ClientHub`, `OopBackend` family, `bootstrap::oop`, `DirectoryService`, and `modkit-transport-grpc`. Persistence rides `modkit-db`.

#### Host Stays Thin

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-principle-thin-host-preserved`

The plugin emits index / timeline events through the `ServerlessRuntimeClient`; the host never reaches into the plugin's checkpoint payloads, eventing internals, or registry tables. This preserves the parent thin-host decision ([ADR-0005](../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)).

#### Cross-Plugin Composition Is the Host's Job

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-principle-cross-plugin-via-host`

Calls into other plugins (e.g. a DSL/Temporal callable hosted in a separate fat plugin) route through the host's plugin-dispatch, never through this plugin's in-plugin router. The in-plugin router only resolves callables registered with this plugin's Callable Registry.

### 2.2 Constraints

#### Satisfies the SDK Plugin Trait

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-constraint-sdk-plugin-trait`

The plugin must satisfy the `RuntimeAdapter` trait from `serverless-runtime-sdk`. That contract is authoritative for host-facing behavior; this DESIGN does not redefine it. The plugin's external surface is exactly this trait plus the `ServerlessRuntimeClient` — no first-party REST/gRPC endpoints, no out-of-band host APIs.

#### Canonical ModKit Layout

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-constraint-modkit-layout`

The plugin must follow the canonical ModKit DDD-light module layout from `docs/modkit_unified_system/02_module_layout_and_sdk_pattern.md` (contract / API / domain / infra layering, SDK pattern, error handling, testing conventions). Internal architecture freedom does not extend to bypassing the standard layout.

#### No Compile-Time Dependency on Host

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-constraint-no-host-compile-dep`

The plugin must not link the host crate at compile time. The only crate dependency on the host side is the SDK contract crate (`serverless-runtime-sdk`). All runtime resolution is through `ClientHub`.

#### Single-Binary In-Process and Managed-OoP

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-constraint-single-binary`

The plugin must function as part of a single host binary running in-process; the managed-OoP variant must be configuration of the same binary, not a separate codebase. The managed-OoP entrypoint runs the same plugin module under `bootstrap::oop::run_oop_with_options`.

## 3. Technical Architecture

### 3.1 Domain Model

The plugin defines no first-class domain entities that cross the host boundary — those belong to the host module ([serverless-runtime DESIGN §3.1](../../../docs/DESIGN.md#31-domain-model): `Function`, `Workflow`, `InvocationRecord`, `Schedule`, `Trigger`, `TenantRuntimePolicy`, etc.). The plugin consumes those entities through the `RuntimeAdapter` trait and emits source events the host materializes into its invocation index.

The plugin's **internal** domain — visible only to embedded executors via `ExecutionContext` and to the plugin's own components — consists of the following concepts. None of these types crosses the host boundary; they are deliberately private to the plugin's internal SDK (working name `cyberware-composed-runtime-embedded-sdk`).

| Concept | Role | Owner |
|---------|------|-------|
| `CallableRecord` | `(callable_id, executor_id, deployment_mode, checkpoint_schema_id, traits)` row. Populated at registration; resolved at dispatch. | Callable Registry |
| `EmbeddedExecutor` (trait) | Single integration point for any embedded language. Methods: `id()`, `checkpoint_schema_id()`, `invoke(ctx, request)`. | Embedded Executor Registry |
| `ExecutionContext` (trait) | Per-invocation handle exposed to embedded executors. Surfaces identity, replay, checkpointing, eventing, sync+async invocation, plugin-managed services. | `ExecutionContext` factory inside the plugin |
| `CheckpointEnvelope` | `(label, schema_id, payload, attempt, created_at)`. Opaque payload owned by the writing executor. | Checkpoint Store |
| `InvocationState` (plugin-internal) | State-machine snapshot for the current invocation: status (`running` / `suspended_waiting_event` / `succeeded` / `failed` / `compensating` / `cancelled`), attempt count, suspension reason, last checkpoint label. | Invocation Engine |
| `EventSubscription` | `(invocation_id, filter, expires_at)`. Registered when an executor calls `ctx.wait_event`. | Event Hub |
| `ScheduleFiring` | A materialized cron / interval firing waiting for dispatch. | Scheduler |

The two layers of compensation defined by the host's WorkflowTraits model (see [serverless-runtime DESIGN §1.4.2](../../../docs/DESIGN.md#142-plugin-model)) are realized as: **step-level** compensation owned by each embedded executor (Starlark `ctx.steps.define(..., compensate=...)`, native Rust per-step closure), and **function-level** compensation declared on the callable and enforced by the plugin's Invocation Engine on terminal failure.

### 3.2 Component Model

The plugin is organized into the following components, all in-process to the plugin module (the same components run inside the managed-OoP child instance when configured).

#### In-Plugin Router

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-component-router`

GTS-keyed dispatcher. Resolves `callable_id → (executor_id, mode)` via the Callable Registry, then resolves the embedded executor instance via `ClientHub::get_scoped::<dyn EmbeddedExecutor>(ClientScope::gts_id(callable_id))`. Issues each dispatch with a freshly built `ExecutionContext`, applies tenant scoping, and emits an OpenTelemetry span. Mode-transparent — caller code is identical for in-process and managed-OoP placement.

#### Callable Registry

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-component-callable-registry`

Map from callable GTS ID to `(executor_id, deployment_mode, checkpoint_schema_id, traits)`. Populated at module start from durable definitions; supports dynamic registration for user-uploaded Starlark and hot-loaded native libraries.

#### Embedded Executor Registry

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-component-executor-registry`

Map from executor ID to `Arc<dyn EmbeddedExecutor>`. In-process embedded executors are constructed at plugin init. Managed-OoP variants are represented by `modkit-transport-grpc` clients to child-process instances of this plugin, registered under the same trait type via `ClientHub`.

#### Invocation Engine

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-component-invocation-engine`

Owns the invocation lifecycle state machine (`running` / `suspended_*` / `succeeded` / `failed` / `compensating` / `cancelled`), retry policy application, wall-clock and resource-limit enforcement, and two-layer compensation (step-level via embedded executors, function-level on terminal failure). Emits timeline events through the `ServerlessRuntimeClient` at every state transition.

#### Scheduler

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-component-scheduler`

Materializes cron / interval schedule firings, applies missed-fire and concurrency policies, and dispatches firings into the in-plugin router as invocations. Schedule definitions live in `composed_runtime_schedule`; pending firings in `composed_runtime_schedule_firing`.

#### Event Hub

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-component-event-hub`

Plugin-local event-bus client. Registers subscriptions when an executor calls `ctx.wait_event`; matches broker deliveries to suspended invocations and resumes them from their checkpoint envelopes; forwards `ctx.emit_progress` notifications as timeline events to the host's invocation index via the `ServerlessRuntimeClient`.

#### Checkpoint Store

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-component-checkpoint-store`

Durable per-`(invocation_id, label)` row store for checkpoint envelopes (see §3.6 Database schemas & tables). Backed by `modkit-db`. Payload bytes are opaque to the plugin; each embedded executor owns its own `schema_id` and serialization.

#### ExecutionContext Factory

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-component-execution-context-factory`

Builds a fresh `ExecutionContext` for each dispatch, wiring identity (tenant, security, callable_id, invocation_id), replay state (is_replay, attempt), trace context, checkpoint/event/invoke methods bound to the appropriate plugin components, and typed `host_service` accessors via `ClientHub`.

#### SDK Plugin Trait Implementation

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-component-sdk-plugin-impl`

The external-facing layer: implements the `RuntimeAdapter` trait from `serverless-runtime-sdk`. Receives invocation, scheduling, event-trigger, lifecycle, and policy calls from the host's plugin-dispatch; routes each into the appropriate plugin component. Emits index/timeline events back to the host through the `ServerlessRuntimeClient`.

#### Managed-OoP Boundary

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-component-managed-oop-boundary`

Spawns and supervises a child-process instance of the plugin when one or more callables are configured for `managed_oop_*` placement. Uses `OopBackend` / `LocalProcessBackend` / `K8sBackend` / `StaticBackend` for supervision; exposes the child instance's embedded executors over `modkit-transport-grpc`; registers them with `ClientHub` under the same `dyn EmbeddedExecutor` trait so the in-plugin router resolves them transparently.

#### Component layout

The components above arrange as follows in the plugin module (all in-process to the plugin, same layout in the managed-OoP child instance):

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                     cyberware-composed-runtime-plugin                        │
│                                                                              │
│  ┌──────────────────────────────────────────────────────────────────────┐    │
│  │  SDK Plugin Trait Impl (registers with host via ClientHub)            │   │
│  │   ◦ register/deregister callables    ◦ tenant policy                  │   │
│  │   ◦ start/suspend/resume/cancel     ◦ schedule + event-trigger CRUD   │   │
│  │   ◦ health/readiness                ◦ timeline event-port emit        │   │
│  └─────────────────────┬─────────────────────────────────────────────────┘   │
│                        │                                                     │
│  ┌─────────────────────▼──────────────────────┐  ┌──────────────────────┐    │
│  │   Invocation Engine                        │  │   Scheduler          │    │
│  │   • lifecycle state machine                │  │   • cron / interval  │    │
│  │   • retry policy                           │  │   • missed-fire pol. │    │
│  │   • timeout enforcement                    │  │   • concurrency pol. │    │
│  │   • two-layer compensation                 │  └──────────┬───────────┘    │
│  └───────────┬────────────────────────────────┘             │                │
│              │                                              │                │
│  ┌───────────▼──────────────────────────────────────────────▼─────────┐      │
│  │   In-Plugin Router                                                 │      │
│  │   • Callable Registry  (gts_id → executor_id, mode, schema, ...)   │      │
│  │   • Executor Registry  (executor_id → Arc<dyn EmbeddedExecutor>)   │      │
│  │   • Lookup: ClientHub::get_scoped::<dyn EmbeddedExecutor>(scope)   │      │
│  │   • Tenant scoping + trace emission per dispatch                   │      │
│  └─┬────────────────────────────┬───────────────────────┬─────────────┘      │
│    │                            │                       │                    │
│  ┌─▼────────────────┐  ┌────────▼────────┐    ┌─────────▼──────────┐         │
│  │  ExecutionContext│  │  Event Hub      │    │  Checkpoint Store  │         │
│  │  factory         │  │  • wait_event   │    │  • write_checkpoint│         │
│  │  • per-invocation│  │  • emit_progress│    │  • read_checkpoint │         │
│  │    ctx instance  │  │  • broker subs  │    │  • backed by db    │         │
│  └─┬────────────────┘  └───┬─────────────┘    └────────────────────┘         │
│    │                       │                                                 │
│  ┌─▼───────────────────────▼───────────────────────────────────────────┐     │
│  │   EmbeddedExecutor implementations  (loaded at init or dynamically) │     │
│  │   ┌────────────┐  ┌──────────────────┐  ┌─────────────────────┐     │     │
│  │   │  Starlark  │  │  Native Rust     │  │  (future) CEL etc.  │     │     │
│  │   │  ADR-0007  │  │  ADR-0008        │  │                     │     │     │
│  │   └────────────┘  └──────────────────┘  └─────────────────────┘     │     │
│  └─────────────────────────────────────────────────────────────────────┘     │
│                                                                              │
│  ┌──────────────────────────────────────────────────────────────────────┐    │
│  │   Managed-OoP Boundary (per-callable opt-in)                         │    │
│  │   Same plugin binary as a child process; in-plugin router resolves   │    │
│  │   to a modkit-transport-grpc client via ClientHub when mode is OoP.  │    │
│  └──────────────────────────────────────────────────────────────────────┘    │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 3.3 API Contracts

This section enumerates the contracts that cross the plugin's two integration surfaces — the external (host-facing) plugin trait surface, and the internal (embedded-executor-facing) trait surface. Wire schemas (JSON Schemas / GTS schema IDs) for the per-callable `params` / `returns` / event payloads belong to each callable's definition; they are not redefined here.

| Contract | Surface | Owner | Consumers |
|----------|---------|-------|-----------|
| `RuntimeAdapter` trait | External (host ↔ plugin) | `serverless-runtime-sdk` (TBD) | Host plugin-dispatch (caller); this plugin (implementer) |
| `ServerlessRuntimeClient` | External (plugin → host) | `serverless-runtime-sdk` (TBD) | This plugin (emitter); host invocation index (subscriber) |
| `EmbeddedExecutor` trait | Internal (plugin ↔ embedded executor) | This plugin's internal SDK | Each embedded-executor crate (Starlark, native Rust, future CEL) |
| `ExecutionContext` trait | Internal (plugin → embedded executor) | This plugin's internal SDK | Each embedded-executor implementation; called per invocation |

The two internal-surface traits are sketched below; the external `RuntimeAdapter` trait is owned by the parent SDK and is not redefined here.

#### 3.3.1 `EmbeddedExecutor` trait

The single integration point for new embedded languages.

```rust
pub trait EmbeddedExecutor: Send + Sync {
    /// Identifier for the executor (e.g. "starlark", "native-rust", "cel").
    fn id(&self) -> &str;

    /// GTS schema ID for this executor's checkpoint envelope payloads.
    fn checkpoint_schema_id(&self) -> &GtsId;

    /// Dispatch one invocation. Receives the unified ExecutionContext and the
    /// request value; returns a response or surfaces a suspension via wait_event.
    fn invoke(
        &self,
        ctx: Arc<dyn ExecutionContext>,
        request: Value,
    ) -> BoxFuture<'_, Result<Value, EmbeddedExecutorError>>;
}
```

Adding a new embedded language is one `impl EmbeddedExecutor` plus a registration call at plugin init — checkpointing, eventing, routing, tenant scoping, trace, and managed-OoP transparency come for free.

#### 3.3.2 `ExecutionContext` trait

```rust
pub trait ExecutionContext: Send + Sync {
    fn invocation_id(&self) -> InvocationId;
    fn callable_id(&self) -> &GtsId;
    fn tenant_id(&self) -> &TenantId;
    fn security_context(&self) -> &SecurityContext;
    fn trace(&self) -> &TraceContext;

    fn is_replay(&self) -> bool;
    fn attempt(&self) -> u32;

    fn read_checkpoint(&self, label: &str) -> Option<CheckpointEnvelope>;
    fn write_checkpoint(
        &self,
        label: &str,
        schema_id: &GtsId,
        payload: Bytes,
    ) -> Result<(), CheckpointError>;

    fn wait_event(&self, filter: EventFilter) -> BoxFuture<'_, Result<Event, EventError>>;
    fn emit_progress(&self, payload: ProgressPayload) -> Result<(), EventError>;

    fn invoke(
        &self,
        callee: &GtsId,
        request: Value,
    ) -> BoxFuture<'_, Result<Value, InvokeError>>;

    /// Typed access to plugin-managed services (HTTP, connection pools, etc.)
    /// resolved through ClientHub.
    fn host_service<T: ?Sized + 'static>(&self) -> Option<Arc<T>>;
}
```

#### 3.3.3 `CheckpointEnvelope`

```rust
pub struct CheckpointEnvelope {
    pub label: String,
    pub schema_id: GtsId,
    pub payload: Bytes,
    pub attempt: u32,
    pub created_at: DateTime<Utc>,
}
```

The plugin stores `payload` opaquely; only the embedded executor that wrote it knows how to deserialize. Schema ID mismatch on `read_checkpoint` is a hard error surfaced through `CheckpointError`.

### 3.4 Mode Resolution and Managed-OoP Dispatch

Per-callable deployment mode is one of:

- `in_process` (default)
- `managed_oop_local_process` — local child process
- `managed_oop_k8s` — child process in a Kubernetes pod
- `managed_oop_static` — pre-provisioned external process

Resolution path on each dispatch:

1. Router reads `(executor_id, mode)` from the Callable Registry by `callable_id`.
2. If `mode == in_process`: dispatch to `Arc<dyn EmbeddedExecutor>` from the Executor Registry directly.
3. If `mode == managed_oop_*`: resolve `dyn EmbeddedExecutor` via `ClientHub::get_scoped::<dyn EmbeddedExecutor>(ClientScope::gts_id(callable_id))`; the resolved instance is a `modkit-transport-grpc` client to the corresponding child-process instance of this plugin.
4. Wrap the dispatch in an `ExecutionContext` carrying tenant + security + trace + replay state.
5. Emit a router span via OpenTelemetry. Propagate context over gRPC for managed-OoP hops.

Changing a callable's mode requires re-registration; for `managed_oop_*` it also requires the corresponding child-process supervisor to be running (spawned via `OopBackend`).

### 3.5 Interactions & Sequences

#### 3.5.1 Sync request/response invocation (API-driven)

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-seq-sync-invocation`

```
Caller (host) ──[plugin-dispatch]──▶ Composed Runtime plugin
                                     │
                                     │ `RuntimeAdapter` trait: start_invocation
                                     ▼
                                     Invocation Engine
                                     │  ◦ allocate invocation_id
                                     │  ◦ build ExecutionContext (is_replay=false)
                                     │  ◦ emit "started" timeline event → `ServerlessRuntimeClient`
                                     ▼
                                     In-Plugin Router
                                     │  ◦ lookup (executor, mode)
                                     │  ◦ tenant scoping check
                                     │  ◦ open OTel span
                                     ▼
                                     EmbeddedExecutor::invoke(ctx, request)
                                     │
                                     ▼
                                     Response
                                     │  ◦ emit "completed" timeline event → `ServerlessRuntimeClient`
                                     │  ◦ close span
                                     ▼
Caller (host) ◀───────────────────── Composed Runtime plugin
```

#### 3.5.2 Event-driven async with suspend/resume

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-seq-event-driven-suspend-resume`

```
Trigger firing ──▶ `RuntimeAdapter` trait: event_triggered
                   │
                   ▼ start (as above) up through EmbeddedExecutor::invoke
                   │
EmbeddedExecutor calls ctx.write_checkpoint("after_step1", schema_id, payload)
                   │  ──▶ Checkpoint Store row inserted
                   │  ──▶ "checkpoint" timeline event → `ServerlessRuntimeClient`
EmbeddedExecutor calls ctx.wait_event({ event_type: ..., filter: ... })
                   │  ──▶ Event Hub registers subscription
                   │  ──▶ "suspended" timeline event → `ServerlessRuntimeClient`
                   │  ──▶ ExecutionContext future yields; invocation parked
                   ▼

[time passes; broker delivers matching event]

Event Hub matches subscription
                   │  ──▶ load invocation state
                   │  ──▶ build new ExecutionContext (is_replay=true, attempt=N+1)
                   │  ──▶ resume EmbeddedExecutor::invoke (executor replays from ctx.read_checkpoint)
                   │  ──▶ "resumed" timeline event → `ServerlessRuntimeClient`
                   ▼
                   completion as in §3.5.1
```

#### 3.5.3 Cross-callable invocation through the in-plugin router

- [ ] `p1` - **ID**: `cpt-cf-composed-runtime-plugin-seq-cross-callable-invocation`

```
Starlark callable C₁ runs; invokes ctx.invoke(C₂_id, input)
                   │
                   ▼
                   ExecutionContext::invoke
                   │
                   ▼
                   In-Plugin Router
                   │  ◦ lookup (executor for C₂, mode for C₂)
                   │  ◦ open child OTel span (parent: C₁'s span)
                   ▼
                   EmbeddedExecutor::invoke(ctx', input)     (e.g. Native Rust)
                   │
                   ▼
                   response returned to Starlark
```

If `C₂` is in `managed_oop_*` mode, the executor resolved through `ClientHub` is a gRPC client; the call crosses the process boundary transparently.

If a Starlark callable tries to `ctx.invoke` a callable hosted in a **different plugin** (e.g. a DSL/Temporal callable), the Callable Registry lookup fails locally; the request bubbles up to the host's plugin-dispatch via the `RuntimeAdapter` trait. The detail of that fallback is owned by the SDK; this DESIGN does not redefine it.

### 3.6 Database schemas & tables

The plugin owns one durable table managed via `modkit-db` for the checkpoint envelope store, plus a small set of internal tables for plugin lifecycle, event subscriptions, and schedule firings. The host's invocation index is **not** managed by this plugin — the host owns it and is populated through SDK event-port emissions.

#### Checkpoint envelope table

```sql
CREATE TABLE composed_runtime_checkpoint (
    invocation_id   UUID        NOT NULL,
    label           TEXT        NOT NULL,
    schema_id       TEXT        NOT NULL,
    payload         BYTEA       NOT NULL,
    attempt         INTEGER     NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    tenant_id       UUID        NOT NULL,
    PRIMARY KEY (invocation_id, label, attempt)
);
CREATE INDEX composed_runtime_checkpoint_tenant_idx
    ON composed_runtime_checkpoint (tenant_id, invocation_id);
```

Read by `ExecutionContext::read_checkpoint`; written by `write_checkpoint`. Payload bytes are opaque to the plugin — the schema is owned by the embedded executor identified by `schema_id`.

#### Plugin-internal tables (illustrative)

The plugin owns additional persistent state for its own lifecycle. The schemas below are illustrative — exact column sets evolve with the implementation and are not part of any external contract. None of these tables crosses the host boundary.

| Table | Purpose | Key columns |
|-------|---------|-------------|
| `composed_runtime_invocation` | Per-invocation lifecycle row: status (`running` / `suspended_*` / `succeeded` / `failed` / …), attempt count, current_label, callable_id, tenant_id, started_at, last_event_at | `invocation_id PK`, indexed by `(tenant_id, status, last_event_at)` |
| `composed_runtime_event_subscription` | Active `wait_event` subscriptions: invocation_id, filter, expires_at | `(invocation_id, subscription_id) PK`, indexed by `expires_at` |
| `composed_runtime_schedule` | Schedule definitions (cron / interval), missed-policy, concurrency-policy | `schedule_id PK`, indexed by `tenant_id` |
| `composed_runtime_schedule_firing` | Materialized schedule firings awaiting dispatch | `firing_id PK`, indexed by `(schedule_id, scheduled_at)` |
| `composed_runtime_native_library` | Loaded native Rust library metadata for hot-reload tracking (per [ADR-0008](../../../docs/ADR/0008-cpt-cf-serverless-runtime-adr-native-rust-executor.md)) | `(library_id, version) PK` |

All tables carry `tenant_id` where applicable; row access is tenant-scoped through `modkit-db`'s standard `SecureConn` pattern.

### 3.7 Hot Reload (for embedded executors that support it)

Today only the native Rust embedded executor supports hot reload, per [ADR-0008](../../../docs/ADR/0008-cpt-cf-serverless-runtime-adr-native-rust-executor.md). The Composed Runtime plugin participates only by:

- providing the Checkpoint Store on which native-executor hot reload rides,
- providing the Callable Registry which the native executor updates during the drain-load-resume cycle,
- emitting timeline events for drain start, drain end, and resume per affected invocation.

The plugin itself does not have its own hot-reload story; updating the plugin requires a host restart in the in-process case and a child-process restart in the managed-OoP case (handled by the OoP backend's existing supervisor).

### 3.8 Observability

- **Spans:** one per `Router::invoke`. Parent-child relationships follow `ctx.invoke` chains. Cross-process spans propagate via `modkit-transport-grpc`.
- **Metrics (ModKit standard surface):**
  - `composed_runtime_invocations_total{outcome, callable_id, tenant_id}`
  - `composed_runtime_invocation_duration_seconds{callable_id, tenant_id}` (histogram)
  - `composed_runtime_in_flight_invocations{tenant_id}` (gauge)
  - `composed_runtime_suspended_invocations{tenant_id}` (gauge)
  - `composed_runtime_checkpoints_total{operation, callable_id}` (counter for read / write / mismatch)
  - `composed_runtime_event_subscriptions{tenant_id}` (gauge)
  - `composed_runtime_oop_dispatch_total{callable_id, outcome}` (counter)
  - `composed_runtime_managed_oop_supervisor_state{child_id}` (gauge)
- **Timeline events:** emitted to the host via the `ServerlessRuntimeClient` for every state transition (start, checkpoint, suspend, resume, retry, compensate, succeed, fail, cancel).

### 3.9 Errors

Errors raised by this plugin are converted to the host SDK error taxonomy (`serverless-runtime-sdk`) before crossing the plugin boundary, per [serverless-runtime DESIGN §1.4.2](../../../docs/DESIGN.md#142-plugin-model). Backend-native error detail (Starlark stack traces, native panic backtraces, gRPC transport errors) stays inside the plugin and is surfaced through the plugin's timeline-retrieval method, not through the host's domain layer.

Internal error categories (non-exhaustive):

- `CheckpointError` — write conflict, schema-ID mismatch on read, store unavailable.
- `EventError` — broker unavailable, subscription registration failure, timeout.
- `InvokeError` — callable not found in plugin, executor unavailable, tenant policy violation, deserialization failure.
- `EmbeddedExecutorError` — surfaced by each embedded executor with executor-specific subtypes (Starlark runtime error, native panic, etc.).
- `OopTransportError` — gRPC failure on managed-OoP hops; normalized at the router into `InvokeError`.

### 3.10 Reuse of ModKit Primitives

| Concern | ModKit primitive |
|---------|------------------|
| Type-safe in-plugin dispatch and managed-OoP resolution | `ClientHub` (`libs/modkit/src/client_hub.rs`), keyed by `dyn EmbeddedExecutor` and `ClientScope::gts_id` |
| Managed-OoP child-process supervision | `OopBackend` / `LocalProcessBackend` / `K8sBackend` / `StaticBackend` (`libs/modkit/src/backends/`) |
| Managed-OoP bootstrap / heartbeat / shutdown | `bootstrap::oop::run_oop_with_options` (`libs/modkit/src/bootstrap/oop.rs`) |
| Module discovery / registration with the host | `DirectoryService` (`libs/modkit/src/directory.rs`) |
| Cross-process transport for managed-OoP | `modkit-transport-grpc` (`libs/modkit-transport-grpc/`) |
| Durable checkpoint envelope store | `modkit-db` |
| HTTP egress from callables | Outbound API gateway resolved via `ClientHub` |
| Event broker access | Resolved via `ClientHub` |

The plugin adds, on top: the Callable Registry, the Executor Registry, the `EmbeddedExecutor` and `ExecutionContext` traits, the Checkpoint Store schema, the Event Hub, and the drain protocol invoked by hot-loaded native executors.

## 4. Additional Context

### 4.1 Out of Scope (and Why)

- **Temporal / cloud FaaS / third-party orchestrators.** These are separate fat plugins under [ADR-0005](../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md). Hosting them inside this plugin would re-introduce the lowest-common-denominator abstraction the thin-host model was designed to avoid. Cross-plugin composition is the host's job.
- **The host's surfaces.** REST / JSON-RPC / MCP, function registry, audit aggregation, the lightweight invocation index — those belong to `cyberware-serverless-runtime`.
- **Embedded language tooling.** IDE integrations, linters, formatters for Starlark or other embedded languages are out of scope for this plugin and live in their respective ecosystems.
- **Security model (execution identity, secret references, privilege scoping).** Deferred to [NEXT_ADR_SCOPE ADR-9](../../../docs/NEXT_ADR_SCOPE.md). The plugin honors whatever security model the platform settles on; designing it is not this plugin's responsibility.
- **Debug API.** Deferred to [NEXT_ADR_SCOPE ADR-11](../../../docs/NEXT_ADR_SCOPE.md). The plugin's trace stream is the substrate the debug subsystem will subscribe to.

### 4.2 Non-Applicable Domains

- **Privacy Architecture (COMPL-DESIGN-002):** Not applicable — the plugin processes opaque tenant payloads; data classification and privacy controls are owned by the platform.
- **Compliance Architecture (COMPL-DESIGN-001):** Not applicable — the plugin contributes audit events to the platform's compliance surface but does not implement compliance architecture independently.
- **User-Facing / Frontend Architecture (UX-DESIGN-001):** Not applicable — backend plugin with no UI.

## 5. Traceability

| Artifact | Location |
|----------|----------|
| Plugin PRD | [./PRD.md](./PRD.md) |
| Parent module PRD | [../../../docs/PRD.md](../../../docs/PRD.md) |
| Parent module DESIGN | [../../../docs/DESIGN.md](../../../docs/DESIGN.md) |
| Thin-host model (grandparent decision) | [ADR-0005](../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md) |
| Composed Runtime decision (this plugin's architectural ADR) | [ADR-0006](../../../docs/ADR/0006-cpt-cf-serverless-runtime-adr-composed-runtime.md) |
| Starlark embedded executor (hosted by this plugin) | [ADR-0007](../../../docs/ADR/0007-cpt-cf-serverless-runtime-adr-starlark-runtime.md) |
| Native Rust embedded executor (hosted by this plugin) | [ADR-0008](../../../docs/ADR/0008-cpt-cf-serverless-runtime-adr-native-rust-executor.md) |
| Next ADR scope | [../../../docs/NEXT_ADR_SCOPE.md](../../../docs/NEXT_ADR_SCOPE.md) |

This DESIGN directly addresses the following principles and components from the parent module:

- `cpt-cf-serverless-runtime-principle-pluggable-adapters` — implemented as the plugin's internal `EmbeddedExecutor` extensibility surface.
- `cpt-cf-serverless-runtime-principle-impl-agnostic` — caller code is decoupled from embedded-executor identity and from in-process / managed-OoP placement.
- `cpt-cf-serverless-runtime-component-executor` — this plugin is the hosting environment for the Executor component; each embedded executor is one implementation.
- `cpt-cf-serverless-runtime-fr-execution-engine` — durable state and replay are plugin-owned, executor-schema-extensible.
- `cpt-cf-serverless-runtime-fr-execution-lifecycle` — uniform invocation, suspend, resume, cancel via the in-plugin router and `ExecutionContext`.
- `cpt-cf-serverless-runtime-fr-runtime-capabilities` — runtime capabilities (HTTP via outbound gateway, events, audit) are exposed through `ExecutionContext` regardless of embedded executor.
- `cpt-cf-serverless-runtime-fr-trigger-schedule` — schedule and event-trigger firings dispatch through the in-plugin router and surface as invocations to embedded executors.
- `cpt-cf-serverless-runtime-nfr-security` — tenant-scoping and security-context propagation enforced once at the in-plugin router.
- `cpt-cf-serverless-runtime-nfr-reliability` — unified checkpoint envelopes back replay correctness across embedded executors.
- `cpt-cf-serverless-runtime-nfr-tenant-isolation` — the plugin's managed-OoP variant provides OS-level isolation for untrusted callables without changing caller code.
- `cpt-cf-serverless-runtime-nfr-ops-traceability` — single trace plane spans embedded-executor boundaries and the in-process / managed-OoP boundary.
