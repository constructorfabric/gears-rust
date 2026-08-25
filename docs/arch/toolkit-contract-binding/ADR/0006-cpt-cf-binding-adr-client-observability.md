---
status: accepted
date: 2026-07-31
---

# Generated-Client Observability via `tracing`/OpenTelemetry (not a `ContractObservability` trait)

**ID**: `cpt-cf-binding-adr-client-observability`

## Table of Contents

1. [Context and Problem Statement](#context-and-problem-statement)
2. [Decision Drivers](#decision-drivers)
3. [Considered Options](#considered-options)
4. [Decision Outcome](#decision-outcome)
5. [Pros and Cons of the Options](#pros-and-cons-of-the-options)
6. [Consequences](#consequences)
7. [More Information](#more-information)

## Context and Problem Statement

Generated REST/gRPC contract clients (`#[toolkit::rest_contract]`, `#[toolkit::grpc_contract]`) carry
retry, timeout, and error mapping. Without telemetry — a span per call and downstream propagation of
the trace/`request_id` — they are unusable in production and a contract hop breaks the distributed
trace.

The original DESIGN §6 (`cpt-cf-binding-constraint-observability`) specified a dedicated
`ContractObservability` trait plus `parent_span` / `metrics` / `observability` fields on
`ClientConfig`, with a default implementation "backed by the `tracing` and `metrics` crates". That
design was **never implemented**. Meanwhile the platform already standardises on `tracing` +
`tracing-opentelemetry`: `toolkit-http`'s `OtelLayer` (`libs/toolkit-http/src/layers/otel.rs`) creates
an `outgoing_http` client span and injects W3C `traceparent` from `Context::current()`; the
api-gateway continues the trace inbound via `set_parent_from_headers`; the canonical error layer
treats the W3C `trace_id` as the `request_id`.

The question: bake telemetry into generated clients via a bespoke `ContractObservability` abstraction,
or via the existing `tracing`/OTEL stack?

## Decision Drivers

* **Downstream propagation of `request_id`/`span_id`** must be automatic — a contract hop joins the
  distributed trace without callers threading IDs by hand.
* **Idiomatic Rust** — use the ecosystem-standard tracing/OTEL machinery the rest of the codebase
  already uses, rather than a parallel observability abstraction.
* **No new public API surface / no consumer burden** — `hub.get::<dyn Trait>()` consumers must be
  unaffected; nothing new to implement.
* **Least-invasive codegen** — small, debuggable macro output.
* **Do not force heavy dependencies** onto SDK crates that opt out of telemetry.

## Considered Options

* **Option A — `tracing`/OTEL.** The macro emits a per-method `tracing` span in the generated client
  body and instruments the awaited dispatch. Propagation is handled by the existing `toolkit-http`
  `OtelLayer` (`traceparent` from `Context::current()`); gRPC gets an analogous interceptor in a
  follow-up. No `ClientConfig` change.
* **Option B — `ContractObservability` trait (original DESIGN §6).** Add
  `observability: Option<Arc<dyn ContractObservability>>` + `parent_span` + `metrics` to
  `ClientConfig`; the macro emits `on_request_start` / `on_retry` / `on_response` / `on_error` hook
  calls; ship a default `tracing`+`metrics`-backed impl.

## Decision Outcome

Chosen option: **Option A — `tracing`/OpenTelemetry.**

The `#[toolkit::rest_contract]` macro now emits, inside each generated client method, a
`tracing::info_span!` with a baked-in name (`{Trait}.{method}`) and OTel-semantic fields
(`otel.kind = "client"`, `rpc.system`, `rpc.service`, `rpc.method`, `http.method`, `http.route`,
`error`), entered across the awaited dispatch. Because the span is `Context::current()` at send time,
`toolkit-http`'s `OtelLayer` injects **this span's** W3C `traceparent`, so `trace_id`/`span_id`
propagate downstream automatically. `request_id == trace_id` by the existing platform convention.

The span path is routed through a `#[doc(hidden)] pub use tracing as __tracing;` re-export in
`toolkit-contract`, so SDK crates need no direct `tracing` dependency.

There is **no** `ContractObservability` trait and **no** new `ClientConfig` field. DESIGN §6 has been
rewritten to match.

## Pros and Cons of the Options

### Option A — `tracing`/OTEL

* Good: propagation of `span_id`/`trace_id` is automatic via `Context::current()` — the primary
  requirement, for free.
* Good: idiomatic and consistent with `toolkit-http`, api-gateway, `telemetry::init` (all
  `tracing`/`tracing-opentelemetry`).
* Good: zero new public API; consumers and `ClientConfig` unchanged; graceful no-op without a
  subscriber.
* Good: minimal codegen (one span + `.instrument()`).
* Bad: unified per-`error_code` metrics are not free — they come from `toolkit-http` `.with_metrics()`
  (feature-gated) or a couple of `metrics::` calls, not one typed hook.
* Bad: full W3C propagation is active only when the build enables `toolkit-http/otel` (opt-in by
  platform convention).

### Option B — `ContractObservability` trait

* Good: one typed seam for tracing+metrics+logs; `on_error(ProblemDetails)` gives error-code-labelled
  metrics out of the box; pluggable non-`tracing` backends.
* Bad: **still requires OTEL injection underneath** to carry context across the wire — additive cost,
  not a replacement for Option A.
* Bad: grows `ClientConfig`'s public surface (3 fields) and adds a trait consumers may feel obliged to
  implement; the DESIGN itself notes changing these fields later is an API break.
* Bad: a hand-rolled `RequestScope` does not compose across await points the way `tracing::Span` +
  `Instrument` does; more invasive codegen (4 hook calls threaded through the retry loop).
* Bad: a parallel observability abstraction competing with the established `tracing` stack.

## Consequences

* DESIGN §6 (`cpt-cf-binding-constraint-observability`) rewritten to describe the codegen-span
  mechanism; the `ContractObservability`/`ClientConfig` sketch removed.
* Generated REST clients emit a per-method span today (unary fully instrumented; streaming enters the
  span per yielded item — full poll-time parenting of the SSE connect is a follow-up).
* **RED metrics are feature-gated on `otel`.** `toolkit-contract` gains an `otel` feature that forwards
  `toolkit-http/otel`; `runtime::client::build_default_http_client` has two cfg variants, so `otel` on ⇒
  the client calls `.with_metrics(client_type)` (that builder method is `#[cfg(feature = "otel")]` on
  `toolkit-http`) *and* injects W3C `traceparent`, and `otel` off ⇒ neither is compiled in (no
  `opentelemetry` forced onto opt-out SDKs). SDKs opt in via their own `otel` feature. The per-method
  `tracing` span is emitted regardless.
* **gRPC deferred:** a `TraceContextInterceptor` in `toolkit-transport-grpc` (mirroring
  `InternalAuthInterceptor`) + per-method spans in `grpc_contract.rs`, wiring the generated client via
  `with_interceptor`.
* Revisit Option B only if a pluggable non-`tracing` observability backend becomes a hard requirement.

## More Information

- **DESIGN** — §6 Observability: [`../DESIGN.md`](../DESIGN.md)
- Client span emission: `libs/toolkit-contract-macros/src/rest_contract.rs` (`client_span_ctor`)
- `tracing` re-export: `libs/toolkit-contract/src/lib.rs` (`__tracing`)
- Transport span + `traceparent` injection: `libs/toolkit-http/src/layers/otel.rs`,
  `libs/toolkit-http/src/otel.rs`
- Server-side `request_id`/`trace_id` derivation: `libs/toolkit/src/api/canonical_error_layer.rs`
