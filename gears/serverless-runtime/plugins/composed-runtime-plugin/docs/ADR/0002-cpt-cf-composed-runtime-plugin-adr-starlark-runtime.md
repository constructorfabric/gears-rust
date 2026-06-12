---
status: accepted
date: 2026-04-29
---
<!--
 =============================================================================
 ARCHITECTURE DECISION RECORD (ADR) — based on MADR format
 =============================================================================
 PURPOSE: Capture WHY Starlark was chosen as the embedded scripting runtime
 for function and workflow execution in the Serverless Runtime module.

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
# ADR — Starlark Executor (Code-as-Orchestration)


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Starlark (embedded interpreter)](#starlark-embedded-interpreter)
  - [WebAssembly (Wasm)](#webassembly-wasm)
  - [JavaScript (embedded engine)](#javascript-embedded-engine)
  - [Python (embedded)](#python-embedded)
  - [Lua (embedded)](#lua-embedded)
  - [Custom DSL](#custom-dsl)
- [More Information](#more-information)
  - [Goals](#goals)
  - [Non-goals](#non-goals)
  - [Relationship to other ADRs](#relationship-to-other-adrs)
  - [Starlark program structure](#starlark-program-structure)
  - [Strong type system](#strong-type-system)
  - [Runtime API surface](#runtime-api-surface)
  - [Workflow orchestration (`ctx.steps`)](#workflow-orchestration-ctxsteps)
  - [Examples](#examples)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-composed-runtime-plugin-adr-starlark-runtime`

## Context and Problem Statement

Cyber Fabric Serverless Runtime supports functions and workflows defined as GTS-identified JSON Schemas and invoked via a unified invocation API. The platform's thin-host model ([serverless-runtime ADR-0005](../../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md)) places execution machinery inside runtime plugins. The Composed Runtime plugin ([ADR-0001](0001-cpt-cf-composed-runtime-plugin-adr-composed-runtime.md)) implements `RuntimeAdapter` from [`serverless-runtime-sdk`](../../../../serverless-sdk/docs/DESIGN.md) and hosts embedded language executors against an in-plugin GTS-keyed router and a unified `ExecutionContext`; each embedded executor implements the `EmbeddedExecutor` trait, and the plugin can run entirely in-process or in a managed-out-of-process child instance via ModKit's existing OoP / `ClientHub` abstraction. This ADR selects the **code-as-orchestration embedded executor** for that plugin — the language and interpreter authors use to write functions and workflows in code, as opposed to the declarative DSL-on-Temporal path which lives in a separate fat plugin under serverless-runtime ADR-0005 (see [serverless-runtime ADR-0003](../../../../docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md), [serverless-runtime ADR-0004](../../../../docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md)).

The executor must be deterministic (replay-safe), sandboxable (multi-tenant, with third-party code paths), debuggable, and embeddable in the Rust host. It must also slot cleanly into ADR-0001's contracts: every helper that touches the outside world or persistent state ultimately calls into `ExecutionContext`; suspend/resume uses `read_checkpoint` / `write_checkpoint` with a Starlark-owned schema; cross-callable invocation goes through the in-plugin router and reaches fellow embedded callables only (a callee in another plugin — e.g. a DSL/Temporal callable — is **not** reachable via `ctx.invoke`; see [ADR-0001](0001-cpt-cf-composed-runtime-plugin-adr-composed-runtime.md) Non-goals).

This ADR defines the Starlark executor:
- the Starlark program structure (entrypoint contract)
- strong typing rules and runtime validation
- the in-language `ctx.*` surface exposed to Starlark code and its versioning
- asynchronous execution model (promise + await)
- workflow orchestration hooks (steps, retries, compensation, snapshots, event waiting) layered on top of ADR-0001's checkpoint, replay, and eventing primitives
- how the executor is surfaced to the host as `FunctionHandler<I, O>` / `WorkflowHandler<I, O>` instances built by the Composed Runtime plugin
- example definitions and Starlark programs for functions and workflows

This ADR is intentionally aligned with `DESIGN.md` (domain model and invocation APIs), `serverless-runtime-sdk` (host-facing handler contract), and ADR-0001 (the Composed Runtime plugin's router, `ExecutionContext`, and shared state).

## Decision Drivers

* Must provide a deterministic and safe execution model for runtime-created/updated code in a multi-tenant environment
* Must support both short-lived function handlers and long-lived workflow orchestration using a single language surface
* Must enforce schema-based typing and prevent invalid input/output at runtime boundaries
* Must support long-running execution patterns: waiting on events, suspend/resume, and durable snapshots
* Must be embeddable in Rust with minimal host surface area
* Must keep all outbound I/O behind runtime-mediated helpers (no direct sockets / processes / filesystem) — egress flows through the platform's outbound API gateway for security, credential management, and observability
* Must provide a versioned runtime API so code remains forward-compatible
* Must support static validation (parse/AST, policy checks) before execution
* Must be deterministic and replayable for durable workflow resume
* Must support debuggability (breakpoints, stack traces, tracing)
* Must be familiar to humans and LLMs for authoring and review

## Considered Options

* Starlark (embedded interpreter)
* WebAssembly (Wasm)
* JavaScript (embedded engine)
* Python (embedded)
* Lua (embedded)
* Custom DSL

## Decision Outcome

Chosen option: "Starlark (embedded interpreter)", because it uniquely satisfies all decision drivers: it provides a constrained, deterministic, Python-like language that is embeddable in Rust via `starlark-rust`, supports structured typing via `struct(...)` conventions, enables static validation via AST inspection, and minimizes the host attack surface compared to general-purpose runtimes. Its determinism is critical for durable workflows that may be resumed from snapshots.

### Consequences

* Good, because Starlark is deterministic given the same inputs and host-provided functions, which is critical for workflow replay
* Good, because the Rust host controls all I/O through `ctx.*` methods, minimizing the attack surface
* Good, because Starlark syntax is Python-like, making it familiar to developers and suitable for LLM-assisted authoring
* Good, because resource controls (wall-clock, instruction count, memory, CPU) can be enforced at the interpreter boundary
* Good, because static validation via parse/AST allows failing fast on syntax errors and restricted constructs
* Good, because `starlark-rust` (Meta) is a mature, widely-used Rust implementation
* Bad, because Starlark has a smaller ecosystem than JavaScript or Python, limiting third-party library availability
* Bad, because developers unfamiliar with Starlark need to learn its constraints (no exceptions, no threads, restricted built-ins)
* Bad, because workflow orchestration patterns (steps, compensation, event waiting) require runtime-provided primitives rather than native language features

### Confirmation

* Prototype a function and a multi-step workflow using the `main(ctx, input)` contract and the `ctx.*` API surface (HTTP, events, steps, checkpoint, invoke)
* Verify deterministic replay by suspending and resuming a workflow from a checkpoint envelope; confirm `ExecutionContext::is_replay()` and `attempt()` reflect the resume
* Verify resource limit enforcement (instruction count, memory, wall-clock) at the interpreter boundary, with errors mapped to the appropriate `ServerlessSdkError` / `RuntimeErrorCategory` variants
* Verify that each registered Starlark callable is surfaced to the host as a working `FunctionHandler<I, O>` or `WorkflowHandler<I, O>` impl built by the Composed Runtime plugin
* Run the adapter conformance test suite (`cpt-cf-serverless-runtime-sdk-fr-conformance-suite`) against the Composed Runtime plugin with Starlark callables registered, covering invocation status transitions, retry, compensation, suspend/resume, and error taxonomy

## Pros and Cons of the Options

### Starlark (embedded interpreter)

Constrained, embeddable, deterministic scripting language designed for configuration and automation.

* Good, because it can express both function handlers and workflow orchestration using the same `main(ctx, input)` contract
* Good, because orchestration relies on runtime-provided primitives (`ctx.checkpoint`, `ctx.events.wait`, `ctx.steps.run`) keeping suspend/resume as a runtime feature
* Good, because the Rust host controls all I/O, persistence, event waiting, step tracking, and compensation via `ctx.*` methods
* Good, because it allows practical pre-execution validation: parse AST, restrict built-ins, validate `main(ctx, input)` export, validate `ctx.*` call arity
* Good, because determinism flows through `ctx.*` methods which can be recorded/replayed
* Good, because resource limits (wall-clock, instruction count, memory, CPU) are enforceable at the interpreter boundary
* Good, because evaluation naturally exposes execution state (call stack, frames, location) for debugging
* Good, because Python-like syntax is familiar and a strong target for LLM-assisted authoring
* Good, because `starlark-rust` is maintained in the `bazelbuild` ecosystem with involvement from Google and Meta
* Bad, because it has a smaller ecosystem than Python/JavaScript
* Bad, because developers must learn Starlark-specific constraints

### WebAssembly (Wasm)

Portable binary instruction format with strong sandboxing.

* Good, because it provides strong sandboxing and mature tooling
* Good, because it offers good performance
* Bad, because the authoring model is heavier and less ergonomic for orchestration scripting
* Bad, because debugging and host-call surface can be more complex
* Bad, because deterministic replay requires additional conventions beyond the format itself

### JavaScript (embedded engine)

General-purpose scripting language with wide ecosystem.

* Good, because JavaScript is a familiar language with a large ecosystem
* Bad, because it has a larger runtime and attack surface
* Bad, because achieving determinism, resource control, and sandboxing is significantly more complex
* Bad, because host embedding is heavier than Starlark

### Python (embedded)

General-purpose language with high developer productivity.

* Good, because Python is very popular with high productivity
* Bad, because embedding and sandboxing Python is hard
* Bad, because resource controls are complex to enforce
* Bad, because too many dynamic escape hatches exist for a multi-tenant runtime

### Lua (embedded)

Lightweight embeddable scripting language.

* Good, because Lua is embeddable and small
* Bad, because it is less aligned with structured typing via `struct(...)` conventions
* Bad, because validation ergonomics don't match the platform's requirements
* Bad, because ecosystem familiarity varies

### Custom DSL

Purpose-built domain-specific language.

* Good, because it provides maximum control and validation
* Bad, because implementation cost is high
* Bad, because ergonomics tend to be poor
* Bad, because custom DSLs tend to grow into full languages over time

## More Information

### Goals

- Provide a deterministic and safe execution model for runtime created/updated code.
- Make functions and workflows compatible with a single language/runtime surface.
- Enforce schema-based typing and prevent invalid input/output at runtime.
- Support long-running execution patterns: waiting on events, suspend/resume, and snapshots.
- Provide a versioned runtime API so code remains forward-compatible.

### Non-goals

- Defining the full sandboxing / isolation strategy.
- Defining the persistence format for snapshots.
- Defining the complete event transport implementation.

### Relationship to other ADRs

This ADR specifies one of the embedded executors hosted inside the Composed Runtime plugin defined in [ADR-0001](0001-cpt-cf-composed-runtime-plugin-adr-composed-runtime.md). The Starlark executor is the code-as-orchestration option offered by that plugin, parallel to the declarative DSL-on-Temporal path which lives in a separate fat plugin under [serverless-runtime ADR-0005](../../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md). Both produce first-class `Function` and `Workflow` callables registered with the host; embedded-callable composition (Starlark ↔ native Rust ↔ future embedded languages) happens through the in-plugin router. Composition with a callable in *another* plugin (e.g. a DSL/Temporal callable) is not reachable via `ctx.invoke`; it is a host-orchestration concern (see [ADR-0001](0001-cpt-cf-composed-runtime-plugin-adr-composed-runtime.md) Non-goals).

- **[serverless-runtime ADR-0001](../../../../docs/ADR/0001-cpt-cf-serverless-runtime-adr-callable-type-hierarchy.md) (Function | Workflow as sibling peer base types):** Starlark-authored callables register as either `Function` or `Workflow`. Workflow-shaped Starlark programs set `workflow_traits` (compensation, checkpointing, suspension — defined in `DESIGN.md` and reified in the SDK's `WorkflowHandler` / `CompensationInput` / `CompensationTrigger` types); the `ctx.*` primitives in this ADR (`ctx.steps`, `ctx.events.wait`, `ctx.checkpoint`) back those traits in-language.
- **[serverless-runtime ADR-0002](../../../../docs/ADR/0002-cpt-cf-serverless-runtime-adr-jsonrpc-mcp-protocol-surfaces-v1.md) (JSON-RPC 2.0 / MCP protocol surfaces):** Orthogonal. The host owns the protocol surfaces and dispatches into the Composed Runtime plugin via `RuntimeAdapter`; a Starlark callable opts into JSON-RPC and/or MCP through the same trait fields any other callable uses, and the helpers here do not change the invocation surface.
- **[serverless-runtime ADR-0003](../../../../docs/ADR/0003-cpt-cf-serverless-runtime-adr-workflow-dsl.md) (Serverless Workflow DSL):** Parallel authoring path served by a separate plugin. serverless-runtime ADR-0003 standardizes a YAML/JSON DSL for declarative workflow authoring; this ADR standardizes a code-based authoring path in Starlark inside the Composed Runtime plugin. Either may be used to define a `Workflow`; the choice is per-callable and routed by the host to the appropriate plugin.
- **[serverless-runtime ADR-0004](../../../../docs/ADR/0004-cpt-cf-serverless-runtime-adr-temporal-workflow-engine.md) (Temporal as the workflow engine):** Separate fat plugin under [serverless-runtime ADR-0005](../../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md). serverless-runtime ADR-0004 selects Temporal as the durable engine for the DSL-authored workflow plugin. Starlark workflows do **not** run on Temporal — they execute as embedded callables inside the Composed Runtime plugin, with durability provided through ADR-0001's `ExecutionContext::write_checkpoint` (surfaced in-language as `ctx.checkpoint`). The two plugins coexist as peers under the thin-host model; nothing in serverless-runtime ADR-0004 applies to Starlark.
- **[serverless-runtime ADR-0005](../../../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md) (Thin host, fat runtime plugins):** Grandparent decision. The Composed Runtime plugin (which hosts this executor) is one `RuntimeAdapter` implementation under that model and surfaces its callables and timeline events back to the host through `RuntimeAdapter` and `ServerlessRuntimeClient`.
- **[ADR-0001](0001-cpt-cf-composed-runtime-plugin-adr-composed-runtime.md) (Composed Runtime plugin):** Parent decision. The Starlark executor is one embedded executor inside this plugin, implementing the `EmbeddedExecutor` trait against the unified `ExecutionContext`. The plugin (and therefore the Starlark interpreter) runs in-process by default or in the plugin's managed-out-of-process variant — used in particular for third-party / untrusted Starlark code where OS-level isolation is required. Caller code is unchanged across modes. `ctx.invoke(callee_id, input)` is the in-language form of `ExecutionContext::invoke` and dispatches through the in-plugin router; so Starlark code can call native Rust callables, other Starlark callables, or future embedded callables (CEL, etc.) interchangeably.
- **[ADR-0003](0003-cpt-cf-composed-runtime-plugin-adr-native-rust-executor.md) (Hot-loadable native Rust executor):** Fellow embedded executor inside the same Composed Runtime plugin. Starlark code calls native Rust callables through `ctx.invoke` exactly the way it calls any other embedded callable; the in-plugin router resolves whether the native callable runs in-process (linked into a hot-loaded library) or in the plugin's managed-out-of-process variant.

#### Mapping the Starlark surface to the SDK and `ExecutionContext`

Starlark callables ride three concentric layers:

1. **Host ↔ plugin (`serverless-runtime-sdk`).** The Composed Runtime plugin implements `RuntimeAdapter` and, per registered Starlark callable, builds a `FunctionHandler<I, O>` or `WorkflowHandler<I, O>` SDK impl that wraps the Starlark source. When the host calls `RuntimeAdapter::execute`, the plugin constructs an `ExecutionContext` (which carries the SDK's read-only `Context` and `Environment`) and dispatches via the in-plugin router. Index/timeline updates flow back to the host through `ServerlessRuntimeClient`; runtime errors are normalized to `ServerlessSdkError` / `RuntimeErrorCategory`.
2. **Plugin ↔ embedded executor (`EmbeddedExecutor` + `ExecutionContext`).** This is the in-plugin contract from ADR-0001. The Starlark executor implements `EmbeddedExecutor::invoke(ctx, request)`; `ctx` is the unified `ExecutionContext`. SDK identity/deadlines are read via `ctx.sdk_context()`; secrets via `ctx.environment()`. Checkpoint, event-wait, progress, and `invoke` live on the `ExecutionContext` (not on the SDK `Context`).
3. **Executor ↔ user code (the in-language `ctx.*`).** Starlark user code sees a thin in-language wrapper. Every Starlark `ctx.*` call below maps onto a single `ExecutionContext` primitive — the SDK never appears in Starlark code:

| Starlark in-language surface | `ExecutionContext` primitive (ADR-0001) |
|------------------------------|-----------------------------------------|
| `ctx.checkpoint(label)` | `write_checkpoint(label, "<starlark-snapshot-schema-v1>", payload)` |
| `ctx.events.wait(...)` resume | `is_replay()` + `read_checkpoint(label)` plus the plugin's event subscription store |
| `ctx.steps.run(step, ...)` | `write_checkpoint("step:" + name, "<starlark-step-schema-v1>", step_state)` and reverse-order replay on resume |
| `ctx.invoke(callee_id, input)` | `invoke(callee_id, input)` |
| `ctx.env.get(name)` | `environment().get(name)` (SDK `Environment`) |
| `ctx.now()` | recorded once at first call as a `<starlark-now-v1>` checkpoint; replays return the recorded value |
| `ctx.rand(label)` | derived deterministically from `invocation_id + label` — **no checkpoint write**; identical inputs yield identical outputs on replay |
| Resource-limit / cancellation hit | mapped to `EmbeddedExecutorError`, then to `ServerlessSdkError` / `RuntimeErrorCategory::RuntimeLimit` at the plugin boundary |

The Starlark executor owns the `<starlark-*-schema-vN>` schema IDs; the plugin stores the payloads opaquely.

### Starlark program structure

#### Entrypoint
All Starlark functions and workflows expose a `main(ctx, input)` function.

- `ctx` is a runtime-provided context object.
- `input` is the validated input object matching the entrypoint `params` schema.

`main()` returns a value matching the entrypoint `returns` schema or terminates execution via `ctx.exit(...)`.

### Strong type system

#### Source of truth
The source of truth for function/workflow types is the GTS-identified JSON Schema:
- `params` (input)
- `returns` (output)
- `errors` (error envelope types)

#### Validation rules
- On function/workflow registration, the runtime validates schema structure and entrypoint contract compatibility.
- At invocation start, the runtime validates provided `params` against the entrypoint `params` schema before executing Starlark.
- On invocation completion, the runtime validates the returned value against the `returns` schema.
- For workflows, the runtime validates snapshot state and resumption input before continuing.

#### Starlark typing model
Starlark is dynamically typed, but the runtime enforces a strong type system by:
- validating boundary values (input/output)
- validating `ctx.*` method arguments (e.g., `url` must be string)
- validating `ctx.events.wait(...)` against an event schema ID and payload schema

The plugin materializes validated `params` as a Starlark `struct(...)` with fields matching the JSON Schema properties (not a generic dict). This is the contract — `input.customer_id` attribute access works for every Starlark callable. Conversely, callables return ordinary Starlark dicts; the plugin validates them against the `returns` schema before surfacing them through `FunctionHandler::call` / `WorkflowHandler::call`.

All objects returned to the caller are compatible with the JSON Schema types declared in `returns`.
In practice, this means Starlark programs treat returned objects as strongly typed structs:
- no missing required fields
- no additional fields when `additionalProperties` is false
- field types must match schema

### Runtime API surface

All runtime capabilities are exposed as methods and namespaces on the `ctx` argument that every Starlark callable receives. This keeps the global namespace clean, makes "runtime-provided" self-evident at every call site, and groups related operations (`ctx.http.get`, `ctx.events.wait`, `ctx.steps.run`).

#### Versioning

The runtime API is versioned at the **callable level**, not at the helper level. A callable declares `runtime_api_version: 1` in its `traits`; the runtime hands it a `ctx` matching that version. Adding a new method is non-breaking and does not change the version. A breaking change to an existing method bumps the version; older callables continue to receive their original `ctx` shape until they are migrated. This avoids `_v1` / `_v2` clutter on every helper name.

**Surfacing a version change.** When the runtime introduces a new `runtime_api_version` N+1, callables that declare an unsupported version are rejected at registration time with a clear diagnostic (no silent fallback). The previous version is supported in parallel until the deprecation window stated in its release notes elapses; after that, registration of callables on the retired version is rejected. Already-registered callables on the retired version receive a registry-side warning event but continue to execute under their original `ctx` for the deprecation window. There is no in-language version-detection helper — callables declare the version once, declaratively.

#### Common conventions
- All runtime capabilities are accessed through `ctx` (the first parameter to `main(ctx, input)`).
- Methods that perform I/O are async and return a `Promise`.
- Methods that influence control flow (`ctx.exit`, `ctx.checkpoint`) are deterministic.
- To preserve determinism and replayability, Starlark code does not use nondeterministic sources such as `datetime.now()` or `random.random()`. Time and randomness come from `ctx.now()` / `ctx.rand(label)`.

#### Promises (`ctx.await`, `ctx.await_all`)

The runtime provides a minimal async model so a Starlark callable can issue multiple I/O calls in flight at once:

- `Promise<T>` is an opaque runtime object.
- `ctx.await(promise) -> T` waits for one promise.
- `ctx.await_all([p1, p2, ...]) -> [T, ...]` waits for many concurrently and returns results in order.

A `Promise<T>` comes in two flavors:

- **Local-completion promises** — produced by `ctx.http.*`, `ctx.invoke`, `ctx.sleep`, `ctx.steps.run`. They complete inside the current invocation's lifetime; `ctx.await` waits in-process without persisting state.
- **Suspendable promises** — produced by `ctx.events.wait` (and any future helper that registers an event subscription). If awaited, the runtime checkpoints the call frame via `ExecutionContext::write_checkpoint`, releases the worker, and resumes the invocation when the external event arrives. On resume, `ExecutionContext::is_replay()` is true and `attempt()` reflects the resumption.

Authors do not pick the flavor; it follows from which helper produced the promise. The distinction matters only for reasoning about durability and worker occupancy.

#### HTTP (`ctx.http`)

##### Response shape

`ctx.http.get(...)` and `ctx.http.post(...)` resolve to an `HttpResult` value which never raises an exception and can represent either a successful HTTP response or an error. `HttpResult` is a Starlark `struct(...)` so callers use attribute access (`result.ok`, `result.response`, `result.error`).

Successful response:
- `ok` (boolean, `true`)
- `response` (a Starlark `struct(...)`)
  - `status_code` (integer)
  - `headers` (dict)
  - `body` (dict)

Error response:
- `ok` (boolean, `false`)
- `error` (a Starlark `struct(...)`) compatible with the runtime error envelope defined in `DESIGN.md`.
  - for non-2xx upstream responses: `gts.x.core.serverless.err.v1~x.core.serverless.err.http.v1~`
  - for upstream transport failures: `gts.x.core.serverless.err.v1~x.core.serverless.err.http_transport.v1~x.core.serverless.err.timeout.v1~` and `gts.x.core.serverless.err.v1~x.core.serverless.err.http_transport.v1~x.core.serverless.err.no_connection.v1~`
  - for runtime failures: `gts.x.core.serverless.err.v1~x.core.serverless.err.runtime.v1~x.core.serverless.err.timeout.v1~`, `gts.x.core.serverless.err.v1~x.core.serverless.err.runtime.v1~x.core.serverless.err.memory_limit.v1~`, `gts.x.core.serverless.err.v1~x.core.serverless.err.runtime.v1~x.core.serverless.err.cpu_limit.v1~`, and `gts.x.core.serverless.err.v1~x.core.serverless.err.runtime.v1~x.core.serverless.err.canceled.v1~`

If the upstream response body is not JSON, the runtime returns an empty object for `response.body`.

##### `ctx.http.get(...)`

Signature:
- `ctx.http.get(url, headers=None, timeout_ms=None, retry=None, exit_on_error=True) -> Promise<HttpResult>`

Notes:
- `url` is required.
- `headers` is an optional dict.
- `timeout_ms` is optional.
- `retry` is an optional runtime-defined retry policy (e.g., `{"max_attempts": 3, "backoff_ms": 200}`).
- `exit_on_error=True` means the runtime calls `ctx.exit(...)` on non-2xx upstream responses or on upstream transport failures.
- `exit_on_error=False` means the promise resolves to an `HttpResult` and the Starlark code is responsible for handling `ok=False` and/or non-2xx status codes.

**Egress goes through the outbound API gateway.** The runtime does not perform outbound HTTP requests directly. It delegates to the outbound API gateway so that security context, credential management, and internal-to-external token/authorization exchange are applied consistently and all egress traffic is centrally enforced and observed. This is part of why Starlark sits behind `ctx.http.*` rather than offering a `requests`-style library: it forecloses any path to direct sockets.

##### `ctx.http.post(...)`

Signature:
- `ctx.http.post(url, body, headers=None, timeout_ms=None, retry=None, exit_on_error=True) -> Promise<HttpResult>`

#### Sleep (`ctx.sleep`)

- `ctx.sleep(milliseconds) -> Promise<None>`

#### Time and randomness (`ctx.now`, `ctx.rand`)

- `ctx.now() -> string`
- `ctx.rand(label=None) -> string`

`ctx.now()` returns a runtime-provided RFC 3339 timestamp string. The runtime writes a `<starlark-now-v1>` checkpoint on first call (capturing the host clock at that moment) and replays the recorded value on resume; subsequent in-attempt calls return the cached value without writing again. Function-shaped callables are typically single-attempt, so this collapses to a single host-clock read for them.

`ctx.rand(label)` returns a runtime-provided pseudo-random hex string. The value is deterministically derived from `invocation_id` plus the optional `label`; identical inputs yield identical outputs on every replay. This does not require a checkpoint write.

#### Exit (`ctx.exit`)

- `ctx.exit(error) -> None`

`error` is an object compatible with the runtime error envelope.

For workflows, calling `ctx.exit(...)` terminates execution and triggers compensation for previously completed steps:
- the runtime executes all registered compensation actions for steps that completed successfully prior to the `ctx.exit(...)` call
- compensations are executed in reverse step completion order
- the runtime does not invoke compensation for steps that did not complete successfully

#### Events (`ctx.events`)

##### `ctx.events.wait(...)`

Signature:
- `ctx.events.wait(event_type_id, filter=None, timeout_ms=None, exit_on_error=True) -> Promise<EventWaitResult>`

- `event_type_id` is a GTS type ID string ending with `~` (e.g., `gts.x.core.events.event.v1~vendor.app.some.event.v1~`).
- `filter` is a valid event filter query string supported by the event broker.
- `timeout_ms` is a non-negative integer.
- The event payload is validated against the event type schema.

`EventWaitResult` is a Starlark `struct(...)`:
- `struct(ok = True, value = <Event>)` when the event is received
- `struct(ok = False, error = <FaaS Error envelope>)` when the wait fails

When an error occurs while waiting (including a timeout):
- if `exit_on_error=True`, the runtime calls `ctx.exit(...)` with an error envelope
- if `exit_on_error=False`, the promise resolves with `struct(ok = False, error = ...)`

When `timeout_ms` is provided and the timeout occurs, the error envelope id is `gts.x.core.serverless.err.v1~x.core.serverless.err.runtime.v1~x.core.serverless.err.timeout.v1~`.

Waiting on an event is implemented by:
- the runtime suspending the invocation
- registering an event subscription
- resuming execution when the event arrives or timeout occurs

#### Checkpointing (`ctx.checkpoint`)

- `ctx.checkpoint(label) -> None`

Calling `ctx.checkpoint(label)` requests the runtime to capture a durable snapshot of execution state and store it as a checkpoint envelope under `label`. The snapshot includes:
- call stack / instruction pointer
- relevant variable bindings
- workflow runtime metadata needed for resume

The plugin may automatically take checkpoints at safe points, but `ctx.checkpoint(label)` allows user code to request one explicitly. Maps to `ExecutionContext::write_checkpoint` from [ADR-0001](0001-cpt-cf-composed-runtime-plugin-adr-composed-runtime.md) under the Starlark executor's snapshot schema.

#### Cross-callable invocation (`ctx.invoke`)

- `ctx.invoke(callee_id, input) -> Promise<output>`

Routes through the in-plugin router from [ADR-0001](0001-cpt-cf-composed-runtime-plugin-adr-composed-runtime.md), so a Starlark callable can invoke any other embedded callable in the same plugin — Starlark, native Rust, or future embedded languages (CEL, etc.) — without knowing the embedded executor or whether the callee runs in-process or in the plugin's managed-out-of-process variant. `ctx.invoke` does **not** reach a callable hosted in a different plugin — `ExecutionContext::invoke` raises "callable not found" for any callee not registered with this plugin. Cross-plugin composition, when needed, is arranged at the host orchestration layer (the SDK exposes no plugin→host outbound-routing path today); see [ADR-0001](0001-cpt-cf-composed-runtime-plugin-adr-composed-runtime.md) Non-goals.

### Workflow orchestration (`ctx.steps`)

For workflows, the runtime orchestrates typical workflow processing:
- identifying steps
- tracking step status and retries
- scheduling retries
- registering compensation actions
- checkpointing and suspend/resume
- event subscription and event-driven continuation

These are exposed under `ctx.steps`.

#### Compensation model

The Starlark executor supports **step-level compensation** via `ctx.steps.define(name, fn, compensate)`. This is complementary to the platform-managed **function-level compensation** defined in `DESIGN.md` (WorkflowTraits section). If step-level compensation handles the failure, the function-level handler is not invoked. See `DESIGN.md` for the `CompensationContext` schema and the two-layer compensation model.

#### `ctx.steps.define(name, fn, compensate=None)`

Registers a step function and an optional compensation function, returning a step handle for use with `ctx.steps.run`.

If `compensate` is provided, it accepts the step output as its third argument:
- `fn(ctx, step_input) -> step_output`
- `compensate(ctx, step_input, step_output) -> None`

The runtime automatically passes:
- `step_input` to the step function and to the compensation function
- `step_output` to the compensation function

This lets the main action provide context to the compensation by returning it as part of the step output (e.g., a created object ID).

#### `ctx.steps.run(step, input, exit_on_error=True)`

Executes a defined step with runtime tracking, retry policy, and status updates. `step` is the handle returned by `ctx.steps.define`.

- `exit_on_error=True` — the runtime calls `ctx.exit(...)` if the step fails.
- `exit_on_error=False` — the call returns a result object and the Starlark code is responsible for handling `ok=False`.

Returns a result struct:
- `struct(ok = True, value = step_output)` on success
- `struct(ok = False, error = <FaaS Error envelope>)` on failure

Compensation invocation:
- When a workflow terminates unsuccessfully via `ctx.exit(...)` (or a runtime abort that maps to a workflow failure), the runtime executes registered compensations for all previously completed steps.
- Compensations execute in reverse step-completion order.
- Compensation failures do not cause additional compensations to be skipped; they are recorded and surfaced via workflow status and logs.

### Examples

#### Function example #1: two upstream calls and composite output

##### Function definition (GTS schema)
```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "gts://gts.x.core.serverless.function.v1~vendor.app.example.compose_customer_profile.v1~",
  "allOf": [
    {"$ref": "gts://gts.x.core.serverless.function.v1~"}
  ],
  "type": "object",
  "properties": {
    "id": {"const": "gts.x.core.serverless.function.v1~vendor.app.example.compose_customer_profile.v1~"},
    "params": {
      "const": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "customer_id": {"type": "string"}
        },
        "required": ["customer_id"]
      }
    },
    "returns": {
      "const": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "customer": {"type": "object", "additionalProperties": true},
          "orders": {"type": "array", "items": {"type": "object", "additionalProperties": true}}
        },
        "required": ["customer", "orders"]
      }
    },
    "traits": {
      "type": "object",
      "properties": {
        "runtime": {"const": "starlark"},
        "invocation": {
          "type": "object",
          "properties": {"supported": {"const": ["sync", "async"]}, "default": {"const": "sync"}},
          "required": ["supported"]
        },
        "caching": {
          "type": "object",
          "properties": {"max_age_seconds": {"const": 0}},
          "required": ["max_age_seconds"]
        }
      },
      "required": ["runtime"]
    },
    "implementation": {
      "type": "object",
      "properties": {
        "code": {
          "type": "object",
          "properties": {
            "language": {"const": "starlark"},
            "source": {"const": ""}
          },
          "required": ["language"]
        }
      },
      "required": ["code"]
    }
  },
  "required": ["id", "params", "returns", "traits", "implementation"]
}
```

##### Starlark code
```python
# main() is the required entrypoint.

def main(ctx, input):
    customer_id = input.customer_id

    p_customer = ctx.http.get(
        "https://example.crm/api/customers/" + customer_id,
        timeout_ms = 2000,
        retry = {"max_attempts": 3, "backoff_ms": 200},
        exit_on_error = True,
    )

    p_orders = ctx.http.get(
        "https://example.orders/api/orders?customer_id=" + customer_id,
        timeout_ms = 2000,
        retry = {"max_attempts": 3, "backoff_ms": 200},
        exit_on_error = True,
    )

    customer_result, orders_result = ctx.await_all([p_customer, p_orders])

    customer_resp = customer_result.response
    orders_resp = orders_result.response

    orders_body = orders_resp.body
    orders_items = orders_body["items"] if "items" in orders_body else []

    return {
        "customer": customer_resp.body,
        "orders": orders_items,
    }
```

#### Function example #2: handle upstream HTTP error/timeout

This example demonstrates disabling automatic exit and returning a structured result.

```python

def main(ctx, input):
    customer_id = input.customer_id

    p_customer = ctx.http.get(
        "https://example.crm/api/customers/" + customer_id,
        timeout_ms = 500,
        retry = {"max_attempts": 1, "backoff_ms": 0},
        exit_on_error = False,
    )

    customer_result = ctx.await(p_customer)

    if not customer_result.ok:
        return {
            "customer": {"id": customer_id, "status": "unavailable"},
            "orders": [],
        }

    customer_resp = customer_result.response

    if customer_resp.status_code >= 400:
        return {
            "customer": {"id": customer_id, "status": "unavailable"},
            "orders": [],
        }

    return {
        "customer": customer_resp.body,
        "orders": [],
    }
```

#### Example #3: workflow with steps, event waiting, checkpoints, and compensation

##### Workflow definition (high-level)
Workflows use the same entrypoint schema model, but are executed with durable semantics.

##### Starlark workflow code
```python

def step_reserve_inventory(ctx, input):
    p = ctx.http.post(
        "https://example.inventory/api/reservations",
        body = {"sku": input.sku, "qty": input.qty},
        timeout_ms = 2000,
        retry = {"max_attempts": 3, "backoff_ms": 200},
        exit_on_error = True,
    )
    resp = ctx.await(p).response
    return struct(reservation_id = resp.body.get("reservation_id"))


def compensate_release_inventory(ctx, step_input, step_output):
    p = ctx.http.post(
        "https://example.inventory/api/reservations:release",
        body = {"reservation_id": step_output.reservation_id},
        timeout_ms = 2000,
        retry = {"max_attempts": 3, "backoff_ms": 200},
        exit_on_error = False,
    )
    ctx.await(p)
    return None


def main(ctx, input):
    reserve = ctx.steps.define("reserve_inventory", step_reserve_inventory, compensate_release_inventory)

    reservation_result = ctx.steps.run(reserve, input) # exits on error
    reservation = reservation_result.value

    ctx.checkpoint("after_reserve") # checkpoint before waiting for approval

    approval_result = ctx.await(
        ctx.events.wait(
            "gts.x.core.events.event.v1~vendor.app.orders.approved.v1~",
            filter = "payload.reservation_id = " + reservation.reservation_id,
            timeout_ms = 86400000,
            exit_on_error = False, # handle error explicitly below
        )
    )
    if not approval_result.ok:
        if approval_result.error.id == "gts.x.core.serverless.err.v1~x.core.serverless.err.runtime.v1~x.core.serverless.err.timeout.v1~":
            approval_event = None
        else:
            ctx.exit(approval_result.error) # triggers compensation for all completed steps
    else:
        approval_event = approval_result.value

    return {
        "reservation": reservation,
        "approval": approval_event,
    }
```

## Traceability

- **Plugin PRD**: [../PRD.md](../PRD.md)
- **Plugin DESIGN**: [../DESIGN.md](../DESIGN.md)
- **Parent gear PRD**: [../../../../docs/PRD.md](../../../../docs/PRD.md)
- **Parent gear DESIGN**: [../../../../docs/DESIGN.md](../../../../docs/DESIGN.md)

This decision directly addresses the following requirements and design elements:

* `cpt-cf-serverless-runtime-fr-runtime-authoring` — Starlark provides the authoring surface for runtime-created functions and workflows
* `cpt-cf-serverless-runtime-fr-execution-engine` — The Starlark interpreter is the execution engine for Starlark-based entrypoints
* `cpt-cf-serverless-runtime-fr-execution-lifecycle` — `ctx.steps`, `ctx.exit`, and `ctx.checkpoint` implement lifecycle management
* `cpt-cf-serverless-runtime-fr-runtime-capabilities` — `ctx.http.*` and `ctx.events.wait` provide runtime capabilities
* `cpt-cf-serverless-runtime-nfr-security` — Starlark's constrained language surface and interpreter-boundary resource controls support security
* `cpt-cf-serverless-runtime-nfr-reliability` — Deterministic replay and snapshotting support reliability
* `cpt-cf-serverless-runtime-principle-impl-agnostic` — Starlark is one pluggable executor; the domain model remains implementation-agnostic
* `cpt-cf-serverless-runtime-principle-pluggable-adapters` — The Starlark executor registers inside the Composed Runtime plugin as an `EmbeddedExecutor` implementation keyed by callable GTS ID, contributing to the plugin's pluggable embedded-executor extensibility
* `cpt-cf-serverless-runtime-component-executor` — The Starlark runtime is the first embedded-executor implementation of the Executor component inside the Composed Runtime plugin
