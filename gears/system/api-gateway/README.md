# API Gateway Gear

HTTP gateway gear that owns the Axum router and collects typed operation specs to emit a single OpenAPI document.

## Overview

The `cf-gears-api-gateway` crate provides:

- HTTP server host for REST APIs
- Operation registration via `toolkit::api::OperationBuilder`
- OpenAPI document aggregation

## Configuration

```yaml
gears:
  api-gateway:
    config:
      bind_addr: "127.0.0.1:8086"
      enable_docs: true
      cors_enabled: false
      auth_disabled: false
      defaults:
        body_limit_bytes: 16777216  # 16 MiB (default)
        request_timeout: 30s        # default
        # max_request_timeout: 2m   # optional upper bound for route declarations
```

### Request timeout

`defaults.request_timeout` is the deadline for a handler to produce a response,
written as a humantime duration (`"30s"`, `"2m"`, `"1500ms"`). It defaults to
`30s`. A request still running at the deadline is answered with a canonical
`deadline_exceeded` problem (HTTP 504). Zero is rejected at startup. An
optional `defaults.max_request_timeout` caps longer per-route deadlines and
explicit route timeout opt-outs; it must be at least `defaults.request_timeout`.
When unset, route declarations have no additional cap.

The deadline ends once the response headers are returned, so streamed bodies
(SSE, chunked downloads, proxied streams) and upgraded WebSocket connections
are not cut off by it.

### Per-route timeouts

An operation overrides the global deadline with `OperationBuilder`:

- `.request_timeout(duration)` — a route-specific deadline (e.g. a
  synchronous-publish route that legitimately holds the response longer).
- `.no_request_timeout()` — no server-side deadline when the gateway has no
  maximum cap; for long-lived
  streaming routes (SSE, multipart streams) a unary timeout would truncate.
  When `defaults.max_request_timeout` is set, the cap also bounds the handler's
  time to produce response headers; after headers are returned, streamed bodies
  remain unbounded.

The declared value is emitted as the `x-toolkit-request-timeout` `OpenAPI`
extension (`"60s"`, or `"none"`), so an edge gateway reverse-proxying the
route honours the same deadline via its `ProxyRegistry` route table, subject to
the edge gateway's configured `defaults.max_request_timeout`.

## License

Licensed under Apache-2.0.
