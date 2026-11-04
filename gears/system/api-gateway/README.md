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
  api_gateway:
    config:
      bind_addr: "127.0.0.1:8086"
      enable_docs: true
      cors_enabled: false
      auth_disabled: false
      defaults:
        body_limit_bytes: 16777216  # 16 MiB (default)
        request_timeout: 30s        # default
```

### Request timeout

`defaults.request_timeout` is the deadline for a handler to produce a response,
written as a humantime duration (`"30s"`, `"2m"`, `"1500ms"`). It defaults to
`30s`. A request still running at the deadline is answered with a canonical
`deadline_exceeded` problem (HTTP 504). Zero is rejected at startup; there is
no upper bound.

The deadline ends once the response headers are returned, so streamed bodies
(SSE, chunked downloads, proxied streams) and upgraded WebSocket connections
are not cut off by it.

## License

Licensed under Apache-2.0.
