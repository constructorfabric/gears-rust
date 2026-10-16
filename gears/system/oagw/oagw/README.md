# OAGW

Outbound API Gateway gear. Manages upstreams and routes, enforces auth and rate limits, and proxies outbound requests over HTTP, SSE, and WebSocket.

## Overview

The `cf-gears-oagw` gear provides:

- **Upstream management** — CRUD for external upstream services with alias-based resolution
- **Route management** — CRUD for routes with HTTP/gRPC match rules, plugins, and rate limits
- **Proxy pipeline** — alias resolution → authZ → credential injection → rate limiting → HTTP forwarding
- **Plugin system** — per-upstream/route auth plugins (`noop`, `api-key`; extensible)
- **Type provisioning** — reconciles pre-configured upstreams and routes from the types registry: at startup in memory, in a background task on a database
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
- **Usable database**: the runtime applies the OAGW migrations before `init`, and upstreams and routes survive restarts. Upstreams and routes provisioned from the types registry are read-only through the Management API and the SDK, and are reconciled by a background task (see below).
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

### Registry provisioning on a database

With a database, OAGW reconciles the types-registry instances in a background task, so the reconcile never delays startup or crash-loops a pod; the pod serves what the database holds meanwhile. One pod reconciles at a time under a lease in `oagw_registry_state`. Every pod re-checks every `recheck_interval` for the life of the process; when the registry content is unchanged a check is one query. An instance that is invalid or conflicts with a Management API row (for example, it wants an alias an API upstream holds) is skipped, logged at `error`, and counted in `oagw_registry_provisioning_failures_total{kind,reason}`; its stored row keeps serving, and the next check retries it, so fixing the conflict needs no restart. An instance OAGW cannot read at all (a `$id` that does not end in a UUID, or a payload that does not parse) is different: it fails the run until it is fixed in the types registry. A dropped registry upstream that API routes (or a skipped registry route) still use is kept until they are gone; registry routes moving off it stay on it until their move succeeds. Database or dependency outages, and any call that takes longer than 20 s, abort the run, which is retried with backoff from 1 s to 5 min (±20% jitter). The REST healthcheck reports Degraded (never Unhealthy) while the last run is pending, failed or not clean, and healthy once one settles. Alert on `oagw_registry_provisioning_runs_total{outcome=~"not_clean|failed"}` and the failures counter, over a window: a run stopped by a pod shutdown also counts as `not_clean`.

```yaml
gears:
  oagw:
    config:
      registry_provisioning:
        generation: 42          # the deployment's revision; see below
        recheck_interval: 5m    # default 5m, ±20% jitter
```

- **`generation`** (optional integer, at most `i64::MAX`): a monotonic deployment generation. A pod whose generation is older than the one stored by another pod does not reconcile, so an old-release pod that restarts during or after a rollout cannot revert newer registry content. Set it from Helm's `.Release.Revision`, which increases on every `helm upgrade` and `helm rollback`, for example through the environment variable `APP__GEARS__OAGW__CONFIG__REGISTRY_PROVISIONING__GENERATION` or the rendered config file. When unset, the guard is off and a startup warning says so. `kubectl rollout undo` does not bump the revision: its pods are refused; recover with `helm rollback` or a higher `generation`.
- **`recheck_interval`** (humantime, from `1s` to `24h`): how often every pod checks the registry state again.

In memory, the reconcile runs once in `post_init`, before the pod serves; an infrastructure error fails startup. There is no re-check (`recheck_interval` and `generation` are ignored), so a skipped instance stays skipped, and the healthcheck Degraded, until the process restarts. See [ADR-0018 Provisioning from the Types Registry](../docs/ADR/0018-optional-persistence.md#provisioning-from-the-types-registry).

The SQLite and PostgreSQL backends are always compiled in. MySQL is supported when built with the `mysql` feature (`cf-gears-oagw/mysql`); without it, configuring MySQL fails startup. MySQL must be 8.0 or later with `explicit_defaults_for_timestamp=ON` (the default), and its timestamps end in 2038. See [ADR-0018 Optional Persistence](../docs/ADR/0018-optional-persistence.md).

## Features

- `test-utils` — exposes `test_support` with harness, mocks, and request/response helpers for integration tests

## License

Apache-2.0
