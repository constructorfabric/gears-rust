<!--
Created:  2026-05-14 by Constructor Tech
Updated:  2026-05-20 by Constructor Tech
-->
# Decomposition: Serverless Runtime


<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [2.1 Gear Scaffold - HIGH](#21-gear-scaffold---high)
  - [2.2 Function Registry - HIGH](#22-function-registry---high)
  - [2.3 REST Surface - HIGH](#23-rest-surface---high)
  - [2.4 Plugin Dispatcher + Invocation Index - HIGH](#24-plugin-dispatcher--invocation-index---high)
  - [2.5 JSON-RPC Transport - HIGH](#25-json-rpc-transport---high)
  - [2.6 MCP Server - HIGH](#26-mcp-server---high)
  - [2.7 Tenant Policy Management - HIGH](#27-tenant-policy-management---high)
  - [2.8 Audit Aggregation + RFC-9457 Error Mapping - HIGH](#28-audit-aggregation--rfc-9457-error-mapping---high)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->


**Overall implementation status:**

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-status-overall`
## 1. Overview

The Serverless Runtime is decomposed into four crates that map directly to the host/SDK/plugin boundary established by [ADR-0005](../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md) (Thin Host Gear, Fat Runtime Plugins), the contract crate having since been split by audience:

- `serverless-runtime/serverless-sdk/` — **Consumer contract crate** (`serverless-runtime-sdk`). Defines the client trait other gears use to invoke callables and observe runs, the value types it exchanges, and its error taxonomy. Carries nothing plugin-facing. Details live in `serverless-sdk/docs/PRD.md` + `DESIGN.md`.
- `serverless-runtime/serverless-plugin-sdk/` — **Plugin contract crate** (`serverless-runtime-plugin-sdk`). Will define the plugin trait, the notification-only plugin → host event port, the authoring traits, and the adapter conformance harness. Not yet designed: the directory currently holds only `BACKUP-pre-split-sdk-docs/`, a backup of the pre-split SDK documents kept for reference and excluded from validation.
- `serverless-runtime/` — **Host implementation crate** (this DECOMPOSITION). Owns Function Registry, Tenant Policy Manager, REST + JSON-RPC + MCP transports (per [ADR-0002](../../docs/ADR/0002-cpt-cf-serverless-runtime-adr-jsonrpc-mcp-protocol-surfaces-v1.md)), Plugin Dispatcher + invocation index, audit aggregation, and RFC-9457 error mapping. The host MUST NOT depend on any plugin crate at compile time.
- `plugins/serverless-runtime-temporal-plugin/` — **First runtime plugin**, owning the fat plugin tier per ADR-0005. Uses Temporal as the durable execution backend ([ADR-0004](../../docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md)) and the CNCF Serverless Workflow Spec as DSL ([ADR-0003](../../docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md)). Future plugins (Lambda, Azure Durable, Starlark) will sit alongside as siblings under `plugins/`.

This DECOMPOSITION covers **only the host implementation crate** (`serverless-runtime/`). The two contract crates — `serverless-runtime-sdk/` (consumer) and `serverless-runtime-plugin-sdk/` (plugin) — and the Temporal plugin crate (`plugins/serverless-runtime-temporal-plugin/`) are tracked as separate work items and will get their own DECOMPOSITION documents inside their respective crate directories once their docs trees are populated. Consumer-SDK PRD + DESIGN live at `gears/serverless-runtime/serverless-sdk/docs/`; that crate's own DECOMPOSITION has not yet been written. The plugin-facing SDK has no documents yet — `gears/serverless-runtime/serverless-plugin-sdk/BACKUP-pre-split-sdk-docs/` holds only a backup of the pre-split SDK documents, kept for reference and excluded from validation.

The 8 host features below are **ordered by core-first** (§2.1–2.4) followed by **additional layers** (§2.5–2.8: alternative transports, governance, observability). A separate **MVP / Deferred** dimension cross-cuts this ordering:

> **Feature documents.** Only F-01 has one ([`features/gear-scaffold.md`](features/gear-scaffold.md)). F-02 – F-08 are referenced below as "F-0n FEATURE.md" for the tactical detail they will carry, but those documents are not written yet, so the headings below are deliberately unlinked rather than pointing at files that do not exist. Add the link when the document lands.

- **MVP** = §2.1–2.3 (F-01 + F-02 + F-03 with registration endpoints only). The minimum to land the host crate in `main` and prove ToolKit integration. Invocation/schedule/trigger endpoints exist in F-03 but return `503` until F-04 lands.
- **Deferred** = §2.4–2.8. All deferred features are p1 by business priority but do not block MVP. Per-feature unlock criteria + tactical detail live in each feature's `FEATURE.md`; the high-level dependency graph is in §3.

## 2. Entries

### 2.1 [Gear Scaffold](features/gear-scaffold.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-feature-gear-scaffold`

- **Purpose**: Bootstrap the `serverless-runtime` host crate as a ToolKit gear. Foundation for every other host feature. Concrete macro/file-layout choices in F-01 FEATURE.md.

- **Depends On**: None

- **Scope**: Bootstrap the host crate as a ToolKit gear — Cargo workspace wiring, ToolKit gear registration, baseline layer skeleton, baseline error type, smoke test of gear loading. Concrete file layout + macro choices + test harness shape live in F-01 FEATURE.md.

- **Out of scope**:
  - Any feature-specific code (REST endpoints, persistence, plugin dispatch, etc. — all later features)
  - SDK crate scaffolding (covered by the SDK crate's own future DECOMPOSITION)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-nfr-composition-deps`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-principle-pluggable-adapters`

- **Design Constraints Covered**:

  - None

- **Domain Model Entities**:
  - None (foundation only)

- **Design Components**:

  - None (precedes all components)

- **API**: n/a (scaffold)

- **Sequences**:

  - None

- **Data**: n/a (no persistence)

### 2.2 Function Registry - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-feature-function-registry`

- **Purpose**: Owns callable-definition lifecycle — CRUD, versioning, lifecycle state, host-side schema validation, and version resolution. Single source of truth for function/workflow definition persistence; consults tenant-policy at the dispatch boundary.

- **Depends On**: `cpt-cf-serverless-runtime-feature-gear-scaffold` and the host-owned function/workflow model in [`DESIGN_RUST_TYPES.md`](../../docs/DESIGN_RUST_TYPES.md). The consumer SDK carries invocation types only.

- **Scope**: Persistent function/workflow definition store with CRUD, versioning, lifecycle state, and host-side schema validation. Delegates adapter-specific validation to plugins. Concrete entity schema, state-machine transitions, validation hooks, and version-resolution rules live in F-02 FEATURE.md.

- **Out of scope**:
  - REST endpoint wiring (F-03)
  - Tenant policy enforcement (F-07)
  - Adapter-specific DSL validation (plugin-side; future plugin DECOMPOSITION)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-runtime-authoring`
  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-tenant-registry`
  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-input-security`
  - [ ] `p1` - `cpt-cf-serverless-runtime-nfr-ops-traceability`
  - [ ] `p3` - `cpt-cf-serverless-runtime-nfr-tenant-isolation`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-principle-gts-identity`
  - [ ] `p1` - `cpt-cf-serverless-runtime-principle-unified-callable`

- **Design Constraints Covered**:

  - None

- **Domain Model Entities**: host-owned function/workflow definition types and supporting projections (concrete list in F-02 FEATURE.md and [`DESIGN_RUST_TYPES.md`](../../docs/DESIGN_RUST_TYPES.md)).

- **Design Components**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-component-function-registry`

- **API**: n/a (REST wiring is F-03; this feature exposes the domain service interface only)

- **Sequences**:

  - `cpt-cf-serverless-runtime-seq-invocation-flow`

- **Data**: Function-definition persistence (concrete table/migration in F-02 FEATURE.md).

### 2.3 REST Surface - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-feature-rest-surface`

- **Purpose**: Unified REST surface for the gear's resource model (functions, invocations, schedules, triggers, policy). Handles auth, OData query support, error responses (via F-08), dispatch into Function Registry / Tenant Policy / Plugin Dispatcher.

- **Depends On**: `cpt-cf-serverless-runtime-feature-function-registry` (registration endpoints, MVP). **Deferred deps**: F-04 plugin-dispatcher (unlocks invocation/schedule/trigger endpoints — return `503` until then), F-07 tenant-policy (governance middleware), F-08 audit-error-mapping (RFC-9457 error format). Integration tactics in F-03 FEATURE.md.

- **Scope**: REST surface for the gear's resource model (functions, invocations, schedules, triggers, tenant policy) via ToolKit `OperationBuilder`, with OData query support. Exact endpoint table, HTTP verbs, DTO shapes, and action-suffix conventions live in F-03 FEATURE.md and the canonical table in `DESIGN.md` §3.6.

- **Out of scope**:
  - JSON-RPC handler (F-05)
  - MCP server (F-06)
  - Per-plugin endpoint extensions

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-runtime-authoring`
  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-execution-visibility`
  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-trigger-schedule`
  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-execution-lifecycle`

- **Design Principles Covered**:

  - None

- **Design Constraints Covered**:

  - None

- **Domain Model Entities**: host-owned domain types (function, invocation index, schedule, trigger, tenant policy) and their filter/patch projections; concrete inventory in F-03 FEATURE.md. Only the invocation value types are also published to consumers, via `serverless-runtime-sdk`.

- **Design Components**:

  - None directly (transport-layer feature)

- **API**: REST surface under the gear's standard base path. MVP exposes function-resource endpoints; the remaining resource buckets are wired but return `503` until F-04 / F-07 land. Endpoint table, parameters, and action-suffix conventions in `F-03 FEATURE.md` (canonical: `DESIGN.md` §3.6).

- **Sequences**:

  - `cpt-cf-serverless-runtime-seq-invocation-flow`
- **Data**: n/a (delegated to F-02 / F-07 / F-04)

### 2.4 Plugin Dispatcher + Invocation Index - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-feature-plugin-dispatcher`

- **Purpose**: **Deferred (post-MVP)** — built when the SDK trait surface is concretely-typed AND at least one plugin crate has been scaffolded. Plugin dispatch + host-side invocation index: routes invocation/schedule/trigger requests to the appropriate plugin (resolved by GTS adapter type) and maintains a queryable index of invocations populated by plugin-emitted events. Aggregate queries answered by the index; deep fetches delegate to the plugin.

- **Depends On**: `cpt-cf-serverless-runtime-feature-gear-scaffold`, `cpt-cf-serverless-runtime-feature-function-registry`. Cross-crate dependency on the plugin trait surface (defined in `gears/serverless-runtime/serverless-plugin-sdk/`).

- **Scope**: Dispatcher (resolve plugin by GTS adapter type), invocation-index persistence, event-port handler for plugin emissions, aggregate query helpers, delegation hooks for deep fetches. Concrete dispatcher implementation, index entity schema, event-handler wiring, and SDK-trait integration shape live in F-04 FEATURE.md.

- **Out of scope**:
  - Plugin crate implementation (future plugin DECOMPOSITION)
  - SDK trait definitions (SDK crate)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-runtime-capabilities`
  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-execution-visibility`
  - [ ] `p2` - `cpt-cf-serverless-runtime-nfr-observability`

- **Design Principles Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-principle-pluggable-adapters`
  - [ ] `p1` - `cpt-cf-serverless-runtime-principle-impl-agnostic`

- **Design Constraints Covered**:

  - None

- **Domain Model Entities**: SDK invocation + status + timeline-event types (concrete list in F-04 FEATURE.md).

- **Design Components**:

  - None directly (dispatcher lives inside the function-registry boundary; executor is plugin-owned)

- **API**: n/a (internal — invoked via REST/JSON-RPC/MCP transports)

- **Sequences**:

  - `cpt-cf-serverless-runtime-seq-invocation-flow`

- **Data**: Invocation-index persistence (concrete table/migration in F-04 FEATURE.md).

### 2.5 JSON-RPC Transport - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-feature-jsonrpc-transport`

- **Purpose**: **Deferred (post-MVP — LLM agent transport)** — built after MVP is in main. JSON-RPC 2.0 transport façade over the host's Invocation Engine contract. Adds the standard JSON-RPC protocol features (batch, notifications, streaming responses) as an alternative entry point to REST.

- **Depends On**: `cpt-cf-serverless-runtime-feature-rest-surface`, `cpt-cf-serverless-runtime-feature-plugin-dispatcher`. **Deferred governance/observability dep**: `cpt-cf-serverless-runtime-feature-audit-error-mapping` (F-08 — RFC-9457 error mapping middleware)

- **Scope**: JSON-RPC 2.0 handler gear, transport-gateway integration, error-code mapping. Exact handler shape, batching/notification semantics, streaming response wire format, and error-code table live in F-05 FEATURE.md.

- **Out of scope**:
  - MCP-specific elicitation / sampling / SSE resumability (F-06)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-llm-agent-integration`

- **Design Principles Covered**:

  - None

- **Design Constraints Covered**:

  - None

- **Domain Model Entities**: SDK invocation request/result + error payload types (concrete list in F-05 FEATURE.md).

- **Design Components**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-component-jsonrpc-handler`
  - [ ] `p1` - `cpt-cf-serverless-runtime-component-transport-gateway`

- **API**: JSON-RPC 2.0 transport endpoint within the gear's REST namespace; exact URL in F-05 FEATURE.md.

- **Sequences**:

  - `cpt-cf-serverless-runtime-seq-jsonrpc-invocation`
- **Data**: n/a (transport-only)

### 2.6 MCP Server - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-feature-mcp-server`

- **Purpose**: **Deferred (post-MVP — LLM agent transport)** — built after F-05 (which it builds on top of) or in parallel. MCP server transport for LLM-agent integration. Adds session lifecycle and human-/LLM-in-the-loop primitives (per the MCP spec) on top of the JSON-RPC façade. Dispatches into the Invocation Engine SDK contract.

- **Depends On**: `cpt-cf-serverless-runtime-feature-rest-surface`, `cpt-cf-serverless-runtime-feature-plugin-dispatcher`. **Deferred governance/observability dep**: `cpt-cf-serverless-runtime-feature-audit-error-mapping` (F-08 — RFC-9457 error mapping middleware)

- **Scope**: MCP server gear per current MCP spec, integrated with the transport gateway. Exact MCP protocol-version pin, session-lifecycle semantics, elicitation/sampling wire details, and resumability tactics live in F-06 FEATURE.md.

- **Out of scope**:
  - Plain JSON-RPC 2.0 handling (F-05)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-llm-agent-integration`

- **Design Principles Covered**:

  - None

- **Design Constraints Covered**:

  - None

- **Domain Model Entities**: SDK invocation request/result + error payload types (concrete list in F-06 FEATURE.md).

- **Design Components**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-component-mcp-server`
  - [ ] `p1` - `cpt-cf-serverless-runtime-component-transport-gateway`

- **API**: MCP transport endpoint within the gear's REST namespace; exact URL in F-06 FEATURE.md.

- **Sequences**:

  - `cpt-cf-serverless-runtime-seq-mcp-tool-call`
  - `cpt-cf-serverless-runtime-seq-mcp-elicitation`
  - `cpt-cf-serverless-runtime-seq-mcp-sampling`
- **Data**: n/a (session state held in-memory + invocation_index)

### 2.7 Tenant Policy Management - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-feature-tenant-policy`

- **Purpose**: **Deferred (post-MVP governance layer)** — built when core host flow (F-01..F-04) is functional. Owns tenant runtime policy (quotas, retention, allowed adapters, default limits, idempotency defaults). Enforced at the plugin-dispatch boundary before any plugin call.

- **Depends On**: `cpt-cf-serverless-runtime-feature-gear-scaffold` and `cpt-cf-serverless-runtime-feature-plugin-dispatcher`. F-07 owns the tenant-policy model and inserts its enforcement before plugin dispatch; neither contract SDK owns these types.

- **Scope**: Tenant-policy persistence + CRUD, pre-dispatch enforcement middleware (adapter allowlist, quota check, default-limit injection). Concrete entity schema, enforcement-middleware hooks, and quota-tracking strategy live in F-07 FEATURE.md.

- **Out of scope**:
  - REST wiring (F-03)
  - Per-tenant secret storage (handled by CredStore via Environment)

- **Requirements Covered**:

  - [ ] `p2` - `cpt-cf-serverless-runtime-fr-governance-sharing`
  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-tenant-registry`
  - [ ] `p3` - `cpt-cf-serverless-runtime-nfr-tenant-isolation`
  - [ ] `p1` - `cpt-cf-serverless-runtime-nfr-resource-governance`
  - [ ] `p2` - `cpt-cf-serverless-runtime-nfr-retention`

- **Design Principles Covered**:

  - None

- **Design Constraints Covered**:

  - None

- **Domain Model Entities**: host-owned tenant-policy family (quotas, retention, defaults, idempotency, usage), defined in [`DESIGN_RUST_TYPES.md`](../../docs/DESIGN_RUST_TYPES.md) and detailed in F-07 FEATURE.md.

- **Design Components**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-component-tenant-policy-manager`

- **API**: n/a (REST wiring is F-03)

- **Sequences**:

  - None directly; consulted by `cpt-cf-serverless-runtime-seq-invocation-flow`

- **Data**: Tenant-policy persistence (concrete table/migration in F-07 FEATURE.md).

### 2.8 Audit Aggregation + RFC-9457 Error Mapping - HIGH

- [ ] `p1` - **ID**: `cpt-cf-serverless-runtime-feature-audit-error-mapping`

- **Purpose**: **Deferred (post-MVP observability layer)** — built after core host flow (F-01..F-04); before then errors return in transport-default form and audit events log only locally. Aggregates plugin-emitted audit + lifecycle events and forwards to the platform audit engine. Maps host/SDK errors to RFC-9457 Problem documents uniformly across all transports.

- **Depends On**: `cpt-cf-serverless-runtime-feature-plugin-dispatcher`. Cross-crate dependency on the plugin error model and trace instrumentation (defined in `gears/serverless-runtime/serverless-plugin-sdk/`).

- **Scope**: Audit-event aggregator subscribed to the dispatcher's event port; RFC-9457 Problem-mapping middleware for all transports; basic sensitive-field masking. Exact event schemas, masking rules, and middleware wiring live in F-08 FEATURE.md.

- **Out of scope**:
  - Audit-engine actor itself (external gear)
  - Full sensitive-field annotation system (deferred — see future Security Model ADR)

- **Requirements Covered**:

  - [ ] `p1` - `cpt-cf-serverless-runtime-fr-input-security`
  - [ ] `p1` - `cpt-cf-serverless-runtime-nfr-ops-traceability`
  - [ ] `p1` - `cpt-cf-serverless-runtime-nfr-security`

- **Design Principles Covered**:

  - None

- **Design Constraints Covered**:

  - None

- **Domain Model Entities**: SDK error + timeline-event + validation types (concrete list in F-08 FEATURE.md).

- **Design Components**:

  - None directly (cross-cutting middleware)

- **API**: n/a (middleware)

- **Sequences**:

  - None directly; participates in `cpt-cf-serverless-runtime-seq-invocation-flow` error path

- **Data**: n/a (delegated to audit-engine actor)

## 3. Feature Dependencies

```text
MVP (critical path) — core, ships first:

F-01 cpt-cf-serverless-runtime-feature-gear-scaffold        (foundation)
    ↓
    └─→ F-02 cpt-cf-serverless-runtime-feature-function-registry
            ↓
            └─→ F-03 cpt-cf-serverless-runtime-feature-rest-surface   (registration endpoints only;
                                                                       /invocations, /schedules, /triggers
                                                                       return 503 until F-04 lands)

Deferred — added incrementally after MVP is in main:

Core (deferred only because of cross-crate prerequisites):

F-04 cpt-cf-serverless-runtime-feature-plugin-dispatcher
    ↘ unlocks /invocations, /schedules, /triggers endpoints in F-03
    requires: SDK trait surface concretely-typed AND at least one plugin crate

Additional transports (LLM agents):

F-05 cpt-cf-serverless-runtime-feature-jsonrpc-transport
    ↘ alternative transport over F-03's Invocation Engine contract
    requires: F-04 (so invocations have somewhere to go)

F-06 cpt-cf-serverless-runtime-feature-mcp-server
    ↘ specialization of F-05 (MCP spec = JSON-RPC 2.0 + session lifecycle + elicitation/sampling)
    requires: F-05 (or in parallel with it)

Additional layers (governance + observability):

F-07 cpt-cf-serverless-runtime-feature-tenant-policy
    ↘ governance middleware augmenting F-03 / F-04 (quota / allowed-runtimes enforcement before dispatch)

F-08 cpt-cf-serverless-runtime-feature-audit-error-mapping
    ↘ observability layer augmenting F-03 / F-04 / F-05 / F-06 (RFC-9457 problem-details +
       audit aggregation from plugin events via F-04)
```

**Dependency Rationale**:

- `cpt-cf-serverless-runtime-feature-function-registry` requires `cpt-cf-serverless-runtime-feature-gear-scaffold` (the host crate must exist) plus the host-owned domain type model in [`DESIGN_RUST_TYPES.md`](../../docs/DESIGN_RUST_TYPES.md) so the registry can persist `FunctionDefinition`.
- `cpt-cf-serverless-runtime-feature-rest-surface` requires `function-registry` (CRUD target) for the registration endpoints — that subset is the MVP scope. The invocation/schedule/trigger REST endpoints are documented as part of F-03 but return `503 Service Unavailable` until `plugin-dispatcher` (F-04) lands. F-03 does NOT hard-require F-04; the wiring point for dispatcher integration is reserved in F-03 but stubbed at MVP.
- **F-04 plugin-dispatcher** is the gating Deferred-but-core feature. Unblocks once two cross-crate prerequisites are met: plugin-SDK trait signatures are concretely-typed AND at least one plugin crate is scaffolded + registered. Until then, REST endpoints depending on dispatch return `503`.
- **F-05 jsonrpc-transport** and **F-06 mcp-server** are alternative transports for LLM agents over the same Invocation Engine contract that REST uses. Additive — without them the host still serves REST-only. F-06 is a specialization of F-05; F-05 is normally built first or in parallel.
- **F-07 tenant-policy** is a governance layer consulted at the plugin-dispatch boundary. It augments F-03/F-04 with a pre-dispatch enforcement middleware. Distinct from basic tenant scoping (which is handled by F-02's standard tenant-isolated SeaORM access).
- **F-08 audit-error-mapping** is an observability layer aggregating audit events from the dispatcher's event port (F-04) and applying RFC-9457 problem-mapping uniformly across transports. Until it lands, audit events log locally and errors return in transport-default form (acceptable for development; required for production launch).

**Foundation feature**: `cpt-cf-serverless-runtime-feature-gear-scaffold` (F-01). No upstream deps; start here.

**Cross-crate dependencies** (out of scope of this DECOMPOSITION; will be tracked in the SDK and Temporal-plugin DECOMPOSITIONs when those crates' docs trees are populated):

- Consumer contract crate (`serverless-runtime-sdk`, docs at `gears/serverless-runtime/serverless-sdk/`): the consumer client trait, the invocation value types, and its error taxonomy. Concrete inventory lives in `serverless-sdk/docs/PRD.md` + `DESIGN.md`.
- Plugin contract crate (`serverless-runtime-plugin-sdk`): the plugin trait surface, the plugin → host event port, shared domain types, and the conformance test suite. Not yet designed; no current documents exist for it.
- Temporal plugin crate (`plugins/serverless-runtime-temporal-plugin`, future): concrete `RuntimeAdapter` implementation with Temporal-native primitives so the dispatcher can resolve a real backend.

**Parallelizable**:

- **MVP**: F-02 can start in parallel with cross-crate SDK domain-type work (defines its own SeaORM types). F-03 follows F-02 sequentially.
- **Deferred**: F-04 can start in parallel with the SDK trait-surface finalization. F-05 (JSON-RPC) and F-06 (MCP) can be developed in parallel after F-04. F-07 (tenant policy) and F-08 (audit + error mapping) can be picked up by any developer at any time after F-04 is functional — they don't block each other and don't block the additional-transport work.
