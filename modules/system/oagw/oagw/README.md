# OAGW

Outbound API Gateway module. Manages upstreams and routes, enforces auth and rate limits, and proxies outbound requests over HTTP, SSE, and WebSocket.

## Overview

The `cf-oagw` module provides:

- **Upstream management** — CRUD for external upstream services with alias-based resolution
- **Route management** — CRUD for routes with HTTP/gRPC match rules, plugins, and rate limits
- **Proxy pipeline** — alias resolution → authZ → credential injection → rate limiting → HTTP forwarding
- **Plugin system** — per-upstream/route auth plugins (`noop`, `api-key`; extensible)
- **Type provisioning** — loads pre-configured upstreams and routes from the types registry on startup
- **ClientHub integration** — registers `ServiceGatewayClientV1` for inter-module use

This module depends on `types-registry` and `authz-resolver`.

## Usage

The primary consumer obtains the client from `ClientHub`:

```rust
use oagw_sdk::api::ServiceGatewayClientV1;

let gw = ctx.client_hub().get::<dyn ServiceGatewayClientV1>()?;
```

### Creating an upstream

```rust
let upstream = gw.create_upstream(ctx, CreateUpstreamRequest::builder(
    Server { endpoints: vec![Endpoint { scheme: Scheme::Https, host: "api.openai.com".into(), port: 443 }] },
    "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
).build()).await?;
```

### Proxying a request

```rust
let req = http::Request::builder()
    .method("POST")
    .uri(format!("/{}/v1/chat/completions", upstream.alias))
    .body(Body::from(payload))?;

let resp = gw.proxy_request(ctx, req).await?;
```

## Configuration

```toml
[oagw]
proxy_timeout_secs = 30

[oagw.credentials]
"my-api-key" = "sk-..."
```

## Features

- `test-utils` — exposes `test_support` with harness, mocks, and request/response helpers for integration tests

## License

Apache-2.0
