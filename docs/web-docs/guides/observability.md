---
title: Observability
description: OpenTelemetry tracing, request IDs, health endpoints, and instrumented HTTP clients.
sidebar:
  label: Observability
  order: 6
---

Gears ship a common operational surface: distributed tracing, request-id propagation, health
endpoints, and instrumented outbound HTTP — configured centrally, so every gear behaves the
same. This guide covers the knobs (see `docs/TRACING_SETUP.md` in the framework repo for the
full reference).

## Configure tracing

Tracing is configured in a `tracing:` block. Point it at any OTLP-compatible backend
(Jaeger, Uptrace, Datadog, …):

```yaml
tracing:
  enabled: true
  service_name: "cf-gears-api"
  exporter:
    kind: "otlp_grpc"            # or "otlp_http"
    endpoint: "http://127.0.0.1:4317"
    timeout_ms: 5000
  sampler:
    parent_based_ratio:
      ratio: 0.1                 # 10% — use always_on in dev, a ratio in prod
  resource:
    service.version: "1.0.0"
    deployment.environment: "dev"
  propagation:
    w3c_trace_context: true
```

Samplers: `always_on`, `always_off`, or `parent_based_ratio`. Exporters: `otlp_grpc`
(port 4317) or `otlp_http` (port 4318). Settings can be overridden by environment variables
for production (e.g. `APP__TRACING__EXPORTER__ENDPOINT=...`).

Spin up a local collector to view traces:

```sh
docker run -d --name jaeger -p 16686:16686 -p 4317:4317 -p 4318:4318 \
  -e COLLECTOR_OTLP_ENABLED=true jaegertracing/all-in-one:latest
# UI at http://localhost:16686
```

## Spans, request IDs, and health — for free

With tracing enabled the gateway and toolkit give you, without per-gear wiring:

- a **span per request** (method, route, `request_id`, `trace_id`);
- **W3C trace-context** propagation in and out (`traceparent`);
- a **request id** correlated across logs and injected as a response header
  (`inject_request_id_header`);
- health endpoints **`/health`** and **`/healthz`** exposed by the API Gateway.

## Instrument your own code

Add spans with the `tracing` macros. Handlers and service methods in the examples use
`#[tracing::instrument]` with structured fields:

```rust
#[tracing::instrument(skip(self, ctx), fields(user_id = %id))]
pub async fn get_user(&self, ctx: &SecurityContext, id: Uuid) -> Result<User, DomainError> {
    tracing::debug!("Getting user by id");
    // …
}
```

## Trace outbound HTTP

Build outbound clients through `toolkit-http` with `.with_otel()` (enable the `otel` feature)
so external calls join the trace and carry the `traceparent` header:

```rust
let client = HttpClient::builder()
    .with_otel()
    .timeout(Duration::from_secs(30))
    .build()?;

let bytes = client.get("https://api.example.com/data").send().await?.checked_bytes().await?;
```

:::note[Initialization]
Telemetry is initialized by the server bootstrap from the `tracing:` config — a standalone
server does this in `run_server(config)`. The framework docs describe the config surface in
full; the exact init entry point is part of the bootstrap, not something gears call directly.
:::

## See also

- [Runtime & lifecycle](../../concepts/runtime-and-lifecycle/) — where background work and
  cancellation fit.
- Full reference: `docs/TRACING_SETUP.md` in the framework repository.
