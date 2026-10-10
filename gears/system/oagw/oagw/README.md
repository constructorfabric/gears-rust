# OAGW

Outbound API Gateway gear. Manages upstreams and routes, enforces auth and rate limits, and proxies outbound requests over HTTP, SSE, and WebSocket.

## Overview

The `cf-gears-oagw` gear provides:

- **Upstream management** — CRUD for external upstream services with alias-based resolution
- **Route management** — CRUD for routes with HTTP/gRPC match rules, plugins, and rate limits
- **Proxy pipeline** — alias resolution → authZ → credential injection → rate limiting → HTTP forwarding
- **Plugin system** — per-upstream/route auth plugins (`noop`, `api-key`; extensible)
- **Type provisioning** — loads pre-configured upstreams and routes from the types registry on startup
- **ClientHub integration** — registers `ServiceGatewayClientV1` for inter-gear use

This gear depends on `types-registry` and `authz-resolver`.

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
    "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
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

## Persistence

Upstreams and routes are kept in memory unless the gear has a database. To persist them across restarts, add a `database` section to `gears.oagw`:

```yaml
gears:
  oagw:
    database:
      server: sqlite_users   # or a postgres server from `database.servers`
      file: oagw.db
```

- **No `database` section, or `database: null`**: upstreams and routes are kept in memory and lost on restart. A warning is logged at startup.
- **Usable database**: the runtime applies the OAGW migrations before `init`, and upstreams and routes survive restarts. Upstreams and routes provisioned from the types registry are reconciled on every boot: changed instances are updated, and instances the registry no longer has are removed. They are read-only through the Management API and the SDK.
- **Configured but unusable database** (connection failure, malformed section, migration failure, backend not compiled in): startup fails. OAGW never falls back to in-memory storage.

On PostgreSQL, also set a statement timeout. OAGW puts no deadline on its own queries, and every proxy request reads the database, so a stalled query stalls the requests waiting on it:

```yaml
gears:
  oagw:
    database:
      server: pg_main
      params:
        statement_timeout: "5s"   # PostgreSQL only
```

The SQLite and PostgreSQL backends are always compiled in. MySQL is supported when built with the `mysql` feature (`cf-gears-oagw/mysql`); without it, configuring MySQL fails startup. See [ADR-0018 Optional Persistence](../docs/ADR/0018-optional-persistence.md).

## Features

- `test-utils` — exposes `test_support` with harness, mocks, and request/response helpers for integration tests

## License

Apache-2.0
