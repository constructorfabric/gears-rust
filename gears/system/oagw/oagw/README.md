# OAGW

<!-- toc -->

- [Overview](#overview)
- [Usage](#usage)
  - [Creating an upstream](#creating-an-upstream)
  - [Proxying a request](#proxying-a-request)
- [Configuration](#configuration)
- [Persistence](#persistence)
  - [Registry provisioning on a database](#registry-provisioning-on-a-database)
  - [Proxy configuration cache](#proxy-configuration-cache)
- [Features](#features)
- [License](#license)

<!-- /toc -->

Outbound API Gateway gear. Manages upstreams and routes, enforces auth and rate limits, and proxies outbound requests over HTTP, SSE, and WebSocket.

## Overview

The `cf-gears-oagw` gear provides:

- **Upstream management** — CRUD for external upstream services with alias-based resolution
- **Route management** — CRUD for routes with HTTP/gRPC match rules, plugins, and rate limits
- **Proxy pipeline** — alias resolution → authZ → credential injection → rate limiting → HTTP forwarding
- **Plugin system** — per-upstream/route auth plugins (`noop`, `api-key`; extensible)
- **Type provisioning** — reconciles pre-configured upstreams and routes from the types registry: at startup in memory, in a background task on a database
- **ClientHub integration** — registers `ServiceGatewayClientV1` for inter-gear use

This gear depends on `types-registry` and `authz-resolver`. With a database and the proxy configuration cache enabled (the default) it also needs the cluster gear, through `cluster-sdk` and the `cluster` feature (see [Proxy configuration cache](#proxy-configuration-cache)).

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

On PostgreSQL, also set a statement timeout. OAGW puts no deadline on its own queries, and every proxy request that misses the configuration cache reads the database, so a stalled query stalls the requests waiting on it: for up to 30 s with the cache on (a cache load is dropped after 30 s), without a limit with it off:

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

### Proxy configuration cache

With a database, each replica caches the configuration the proxy reads, so a repeated lookup runs no query. A write evicts the entries it affects, at once on the writing replica and, through a short change log in the cluster gear, on the others; the TTL bounds staleness when a change is lost. The database stays the only store of configuration. See [ADR-0020 Proxy Configuration Cache](../docs/ADR/0020-proxy-config-cache.md).

```yaml
gears:
  oagw:
    config:
      proxy_cache:
        enabled: true      # default; no effect without a database
        ttl: 60s           # humantime, from 1s to 1h
        max_entries: 10000 # per section (aliases, routes); 1 to 1000000
```

- **With a database**, the cluster gear must have an `oagw` profile whose cache supports watches. Without it, startup fails with an embedded cluster; with a remote cluster, OAGW starts and its cluster readiness reports the problem.
- **With a database and `enabled: false`**, there is no cache and no change log, and OAGW needs no cluster gear (nor the `cluster` feature): every request reads the database. Set `enabled` alike on every replica: a replica with the cache off does not publish its writes, so replicas that cache see them only after the TTL.
- **Without a database**, OAGW needs no cluster gear and supports one replica only: each replica's in-memory configuration is its own.
- **Standalone cluster backend**: one replica only. Several replicas need a shared backend (Redis, PostgreSQL, k8s). OAGW does not detect several replicas on a standalone backend: health stays green, and each replica sees the others' writes only after the TTL.
- **Health** (all Degraded, never Unhealthy, unless noted): `proxy_cache_watch_connecting` while the change log watch connects or reconnects; `proxy_cache_watch_down` once it is closed for good or stopped (freshness falls back to the TTL); `proxy_cache_newer_log` while appends skip a change log of a newer format. Cluster readiness is included: a cluster still starting or a degraded profile is Degraded, a misconfiguration Unhealthy. OAGW waits at most 250 ms for the cluster's check, reports its last result while a slow check runs, and after 10 s reports `cluster_readiness_slow`. The worst part sets status and code (the cache before registry provisioning on a tie); the message names every unhealthy part.
- **Rollback**: after rolling back a release that wrote a newer change log format, older replicas stay Degraded with `proxy_cache_newer_log` and skip their appends. Delete the cluster key `oagw/proxy-config-changes`: the next write starts a new log, and every replica empties its cache once.
- **A cancelled write** (its future dropped) is published again 30 s later (while the change feed runs), in case its statement commits late. If that second append fails, it counts as a change of its own in `oagw_proxy_cache_publish_failures_total`.
- **Metrics**: `oagw_proxy_cache_lookups_total{map,result}`, `oagw_proxy_cache_entries{map}`, `oagw_proxy_cache_evictions_total{map,reason}`, `oagw_proxy_cache_changes_total{source}`, `oagw_proxy_cache_flushes_total{cause}`, `oagw_proxy_cache_loads_discarded_total{map}`, `oagw_proxy_cache_publish_failures_total{reason}` (`append_failed`, `newer_log`, `publisher_stopped`; once per change not appended), and lookup time as `oagw_request_duration_seconds{phase="resolve"}`. Alert on publish failures and on flushes with `cause="log_gap"` or `"watch_closed"`.

A standalone `oagw` profile, for one replica:

```yaml
gears:
  cluster:
    config:
      profiles:
        oagw:
          cache: { provider: standalone }
```

For several replicas on Redis:

```yaml
gears:
  cluster:
    config:
      profiles:
        oagw:
          cache: { provider: redis, url: "${OAGW_REDIS_URL}", watch_mode: publish }
          lock: { provider: redis, url: "${OAGW_REDIS_URL}" }
          leader_election: { provider: default, allow_weak_consistency: true }
```

- `watch_mode: publish` (the default) is required; `disabled` fails startup.
- A Redis cache is eventually consistent, so the profile needs the `lock` and `leader_election` lines to start, although OAGW uses neither.
- Run Redis with `maxmemory-policy noeviction`, so it never evicts the change log key.
- On an eventually consistent backend (Redis beyond a single node), two concurrent appends can both succeed and lose one change; the affected entries on other replicas stay stale for at most the TTL.

**Builds.** The host binary decides where the cluster gear runs:

- **Embedded**: the binary turns on `cf-gears-oagw/cluster` and links the `cluster` gear and `grpc-hub`, as the example server's `oagw-embedded` feature does (`oagw-embedded = ["oagw", "api_egress/cluster", "dep:cluster", "grpc-hub"]`).
- **Remote** (the cluster runs in another pod): turn on `cf-gears-oagw/grpc-client`, which includes `cluster`. The SDK derives the cluster service's address from `POD_NAMESPACE`; where it is unset and no cluster gear runs, startup fails, with or without a database, so turn it on only for this case.
- **In memory only**: neither feature. OAGW links no cluster SDK and caches nothing: with a database and the cache enabled, `init` fails; with `proxy_cache.enabled: false`, it runs on the database.
- The `cluster` gear turns on `cluster-sdk/grpc-client` itself. Cargo unifies features within one invocation, so a binary linking `cluster-sdk` built together with one linking the cluster gear (for example `cargo run --bin …` without `-p`) gets it too, and fails startup as above. A binary with OAGW in memory only is not affected.
- **Tests** run in both builds. `make test` (and `cargo nextest run -p cf-gears-oagw`) builds OAGW in memory only and checks that a database is refused at `init`. `make test-integration LANE=oagw-cluster` (`--features cluster`) runs the change log, publisher, watcher and the gear on a database; `make test-integration LANE=oagw-db` (`--features integration`, which includes `cluster`) runs them on PostgreSQL, MySQL and Redis.

The SQLite and PostgreSQL backends are always compiled in. MySQL is supported when built with the `mysql` feature (`cf-gears-oagw/mysql`); without it, configuring MySQL fails startup. MySQL must be 8.0 or later with `explicit_defaults_for_timestamp=ON` (the default), and its timestamps end in 2038. See [ADR-0018 Optional Persistence](../docs/ADR/0018-optional-persistence.md).

## Features

- `test-utils` — exposes `test_support` with harness, mocks, and request/response helpers for integration tests
- `cluster` — the proxy configuration cache's change log on the cluster gear, which OAGW on a database requires; see [Proxy configuration cache](#proxy-configuration-cache)
- `grpc-client` — the `cluster` feature, plus reaching a cluster gear running in another pod over gRPC (`cluster-sdk/grpc-client`)
- `mysql` — compile the MySQL backend in

## License

Apache-2.0
