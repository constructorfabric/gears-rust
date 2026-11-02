---
status: proposed
date: 2026-10-13
---

# Proxy Configuration Cache — In-Memory Cache with a Cluster Change Log

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [What Is Cached](#what-is-cached)
  - [Request Path](#request-path)
  - [Keeping the Cache Fresh](#keeping-the-cache-fresh)
  - [The Change Log](#the-change-log)
  - [Cluster Setup](#cluster-setup)
  - [Failure Handling](#failure-handling)
  - [Configuration](#configuration)
  - [Health and Metrics](#health-and-metrics)
  - [Out of Scope](#out-of-scope)
  - [Relation to Earlier ADRs](#relation-to-earlier-adrs)
  - [Expected Gains](#expected-gains)
  - [Benchmark Plan](#benchmark-plan)
  - [Consequences](#consequences)
  - [Scenarios](#scenarios)
  - [Confirmation](#confirmation)
- [Open Questions](#open-questions)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [In-memory cache, cluster change log, targeted eviction, TTL backstop](#in-memory-cache-cluster-change-log-targeted-eviction-ttl-backstop)
  - [In-memory cache, cluster change signal, full flush](#in-memory-cache-cluster-change-signal-full-flush)
  - [In-memory cache, database generation poll](#in-memory-cache-database-generation-poll)
  - [In-memory cache, TTL only](#in-memory-cache-ttl-only)
  - [Configuration in a shared cache](#configuration-in-a-shared-cache)
  - [No cache](#no-cache)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-proxy-config-cache`

## Context and Problem Statement

With a database configured ([ADR: Optional Persistence](./0018-optional-persistence.md)), every proxied request reads its configuration from the database: three `SELECT`s to find the upstream and the route, and five when a write races the read. Configuration is read on every request. This adds latency to every request, puts load on the database that grows with traffic, and makes proxying fail when the database is slow or down.

OAGW usually runs as several replicas that share one database. A cache on one replica does not see writes made through another replica. How should OAGW cache the configuration the proxy reads, and how do the replicas learn what changed?

The write rate is unknown. Many tenants write through the Management API, so the cache must not depend on writes being rare.

## Decision Drivers

* A cache hit runs no database query.
* A replica sees its own writes at once.
* A replica sees writes made on other replicas within milliseconds when the change is delivered, and always within a fixed time limit.
* A write reloads only the entries it can affect, so frequent writes in some tenants do not lower the hit ratio of the others.
* The database stays the only source of truth. Nothing else stores configuration.
* Results are the same as without the cache: the same upstream, the same route, and the same errors.
* A deployment without a database needs no cluster gear, and a single replica with a database needs no extra infrastructure.
* A lost change never leaves a replica stale forever.

## Considered Options

* In-memory cache, cluster change log, targeted eviction, TTL backstop
* In-memory cache, cluster change signal, full flush
* In-memory cache, database generation poll
* In-memory cache, TTL only
* Configuration in a shared cache (Redis as a second cache level, as in ADR-0007)
* No cache

## Decision Outcome

Chosen option: "In-memory cache, cluster change log, targeted eviction, TTL backstop". Each replica keeps an in-memory cache of the configuration the proxy reads. Every configuration write evicts only the entries it can affect, on the writer at once and on every other replica through a short change log kept in the cluster gear. Each entry also expires after a TTL, so a lost change is corrected within the TTL. When a replica cannot know what it missed (a gap in the log, a new log, a log it cannot read, a watch that fell behind or reconnected), it empties its whole cache.

The cluster gear carries only the change log. It never stores configuration. OAGW requires the cluster gear only when a database is configured and the cache is enabled.

### What Is Cached

The cache serves only the proxy lookup: `resolve_proxy_target` without tags (`Tags::Skip`), which is what the proxy calls. Management reads, writes, write validation, and the SDK's `resolve_proxy_target` with tags read the repositories as before.

The cache is OAGW's own, in `infra/proxy_cache/`. It has two sections, both per replica. Each holds the raw repository answer, not a resolved result:

| Section | Key | Value | Loaded on a miss by |
|---|---|---|---|
| Alias | normalized alias + tenant chain, in order | Every upstream with the alias owned by a tenant in the chain, including disabled ones and ones not visible to the requester; empty when there is none | `list_by_alias_for_tenants` without tags (one `SELECT`) |
| Route | the upstream's tenant and ID (`RowKey`) | The upstream's enabled HTTP routes, in full (path, methods, plugins, priority, rate limit, CORS), without tags | `list_proxy_routes` (one `SELECT` for all the missing upstreams; one more per chunk past the backend's bind-parameter limit) |

* **The values are raw candidates.** Upstream selection (closest visible enabled upstream, "disabled" or "not found"), the SSRF check, `select_route` and the merge run on every request with the uncached code, so a hit returns what an uncached lookup returns, by construction.
* **The key holds the tenant chain, not just the tenant.** The chain is read from the tenant resolver on every request, so a moved tenant uses a new key. Tenant moves need no eviction.
* **Routes are cached per upstream, not per request path**, which is unbounded. `select_route` runs per request over the cached routes.
* **The route section is separate** so an ancestor upstream used by many tenants keeps one copy of its routes. A route's tenant is always its upstream's (composite foreign key), so `RowKey` keeps the secure-ORM tenant scope of the load.
* **"Not found" and "disabled" are cached**, so an unknown alias runs no `SELECT` on repeat. An alias that fails the Management API's alias rules (longer than 253 bytes, or invalid characters) is answered "not found" before the cache, with no read and no entry, so requests cannot flood the alias section.
* **Errors are never cached.** A failed read returns its error to every waiter; the next request tries again.
* **Each section indexes its entries by the IDs they hold**: the alias section by candidate upstream ID and by alias; the route section by route ID. Writes find affected entries through these indexes, which change only with the entries, under the section's lock.
* **Size and expiry.** Each section holds at most `proxy_cache.max_entries` entries; when full, the least recently used entry is evicted, so a hot entry outlives a run of one-off keys (such as unknown aliases) shorter than the section. An entry older than the TTL is a miss, and its lookup removes it. Full keys are stored, so keys never collide.
* **One load per key.** Concurrent misses on a key share one load and its result, error included. The load runs as its own task, so it finishes even if every waiter is cancelled. A load over 30 s is dropped and its waiters get an error. `routes_of` serves hits, joins running loads, and starts one batched load for the rest.
* **Why not `pingora-memory-cache`.** It cannot list its keys and keys entries by a 64-bit hash only, so it cannot support the ID indexes targeted eviction needs.

### Request Path

Today, and with the cache off:

```text
resolve_proxy_target
 ├─ tenant chain (tenant resolver)
 ├─ SELECT upstreams with the alias across the chain            (Q1)
 ├─ pick the closest visible enabled upstream; SSRF check
 ├─ SELECT candidate routes                                      (Q2)
 ├─ select_route
 ├─ SELECT the winning route in full                             (Q3)
 │   (the winner changed in between: Q2 and Q3 once more)
 └─ merge the effective configuration
```

With the cache:

```text
resolve_proxy_target
 ├─ tenant chain (tenant resolver)                         unchanged
 ├─ alias section: hit → candidates                         no SQL
 │                 miss → Q1, then store
 ├─ pick the closest visible enabled upstream               same code, per request
 ├─ SSRF check of the selected upstream                    unchanged, per request
 ├─ route section: hit → routes of the selected upstream    no SQL
 │                       and its merge chain
 │                 miss → one SELECT for the missing upstreams, then store
 ├─ select_route over the cached routes                    same function
 └─ merge the effective configuration                      unchanged
```

A hit runs no `SELECT`; a miss runs one per missed section. Routes are loaded in full by one query, so the Q2/Q3 winner re-check is not needed. The per-request SSRF check means a host denied after caching stops resolving at once.

Without a database, with `proxy_cache.enabled: false`, and for the SDK's lookup with tags, the uncached path runs unchanged.

### Keeping the Cache Fresh

**What a write evicts.** Every configuration write maps to one change, built from the repository call's arguments (no extra read), which evicts exactly the entries it can affect:

| Write | Change | Alias section evicts | Route section evicts |
|---|---|---|---|
| Upstream create, update | upstream `U` in tenant `T`, written with alias `a` | entries holding `U`, and entries for `a` whose chain contains `T` | — |
| Upstream delete (API, registry, `delete_upstream_if_unused`) | upstream `U` in `T` deleted | entries holding `U` | `U`'s entry (the cascade) |
| Route create, update | route `R` on upstream `U` in `T` | — | `U`'s entry, and entries holding `R` |
| Route delete | route `R` in `T` | — | entries holding `R` |
| Routes of an upstream deleted | the routes on `U` in `T` | — | `U`'s entry |
| Anything may have changed | all | everything | everything |

Why this is complete:

* **An alias entry depends only on the upstream rows with its alias in its chain.** A write to `U` either removes or changes a member (update, disable, sharing change, rename away, delete: evict entries holding `U`), or adds one (create, or rename to `a`, which may shadow or un-shadow an ancestor's upstream: evict entries for `a` whose chain contains `T`, "not found" included). An ancestor's write reaches descendants because their chains contain it.
* **A route entry depends only on its upstream's route rows.** A route write adds a route (its upstream's entry), changes or removes one (entries holding it), or moves one (both).
* **Tenant moves and SSRF changes need nothing**: the chain is in the key, and the SSRF check runs per request.

**Writes on this replica.** The repositories are wrapped by decorators, so every write path evicts (Management API, SDK, registry reconcile including `delete_upstream_if_unused`), and a new service method cannot skip it. The change is applied after every write call, successful or not, and also when the write's future is dropped mid-call (a cancelled request, a reconcile call cut at its timeout), because a write can commit before its caller goes away or fail after committing. A statement already sent when dropped (a `DELETE` waiting on a row lock, a `COMMIT`) can complete after some replica reloaded the old row, so a dropped write applies and publishes its change once more 30 s later, while the change feed runs. Later completions, or ones after OAGW begins to stop, are covered by the TTL. A failed second append is counted on its own (`reason="append_failed"`); one after the feed stopped is not counted again.

**Writes on other replicas.** After evicting locally, the writer appends the change to the cluster change log ([The Change Log](#the-change-log)). Every replica watches the log and applies the other replicas' changes to its own cache.

**Order of steps for a write:**

1. Build the change from the call's arguments.
2. Run the write (commit to the database).
3. Evict the affected entries locally.
4. Append the change to the change log, and wait for the append for at most 2 seconds. Writes of a registry reconcile run do not wait each; the run waits once, at its end, for its last write's append.

A request that starts after step 3 reads the new data. A Management API or SDK write answers after step 4, so a client that writes and then calls another replica sees the change there once delivered. Registry reconcile writes have no such client: they hand changes over without waiting, the publisher batches them, and the run waits once for the latest append (appends are acknowledged in order). When the append fails or exceeds 2 s, the write still returns its own result and OAGW logs a warning naming the change. A change is counted as not published only once its fate is known (append failed for good, newer log, publisher stopped); a write that merely stops waiting is not counted, as its append may still land.

**In-flight loads.** A load that started before a write may return pre-write rows. So a change that touches a section also detaches every load in flight there: each load has an ID and stores its result only if it is still the section's load for its key. A detached load still answers its existing waiters but is not stored; a later request starts a new load that reads the committed rows. All loads of the section are detached because a load's rows are unknown until it returns; loads in flight are few, so this is cheap. Entries stored before the change are caught by the eviction rules.

**The TTL.** Every entry expires after `proxy_cache.ttl` (default 60 s), bounding staleness from a lost change.

### The Change Log

A cluster watch event carries no value (the watcher reads it with `get`), and quick writes to one key can coalesce into one event. So the key holds a short log of the latest changes, not a single signal:

```json
{ "v": 1,
  "log": "4f8c…",
  "seq": 1042,
  "changes": [
    { "seq": 1041, "origin": "9a1e…", "c": { "upstream": { "tenant": "…", "id": "…", "alias": "api.openai.com" } } },
    { "seq": 1042, "origin": "9a1e…", "c": { "route_deleted": { "tenant": "…", "id": "…" } } } ] }
```

Each change `c` is one of five variants. The format is a contract between replicas and OAGW versions; a variant is never changed or removed within a version:

| Variant | Fields | Written for | Evicts |
|---|---|---|---|
| `upstream` | `tenant`, `id`, `alias` | upstream create or update (`alias`: the alias written) | entries holding `id`; entries for `alias` whose chain contains `tenant` |
| `upstream_deleted` | `tenant`, `id` | upstream delete | entries holding `id`; the route entry of `(tenant, id)` |
| `route` | `tenant`, `id`, `upstream` | route create or update (`upstream`: the upstream written) | the route entry of `(tenant, upstream)`; entries holding `id` |
| `route_deleted` | `tenant`, `id` | route delete (its upstream is not known) | entries holding `id` |
| `routes_of_upstream` | `tenant`, `upstream` | all routes of an upstream deleted | the route entry of `(tenant, upstream)` |

Emptying the whole cache is never logged: it is a local decision of the replica that cannot know what it missed.

* **Key**: `proxy-config-changes` in the cluster cache of the `oagw` profile, scoped to `oagw` (the backend sees `oagw/proxy-config-changes`). It never expires.
* **`v`** is the format version, **`log`** a random ID set when the log is created, **`seq`** the change number, and **`origin`** a random ID per OAGW process, so a replica skips its own (already applied) changes. The log's own ID and sequence are used, not the cluster entry's version, which restarts at 1 when the key is re-created.
* **Bounded** to the last 128 changes and 32 KiB, whichever is reached first, oldest dropped first (the k8s backend's default value limit is 256 KiB). A replica that falls further behind between two reads empties its cache once; nothing is lost.

**Publisher.** One task per replica takes everything queued at once, so a burst of writes or a reconcile pass goes out in one append (or a few). An append reads the log, adds the changes, and writes it back with a compare-and-swap on the entry's version, creating a new log when the key is absent. A conflict, a retryable cluster error, or an attempt (one read plus one conditional write) over 1 s is retried, up to 5 attempts, with jittered backoff from 20 ms doubling; a hung cluster fails an append after about 5 s. A timed-out attempt may still land, so a retry can log changes twice, which is harmless. A malformed or older-format log is replaced by a new log with a new ID, so its readers empty their caches. A newer-format log is left alone: the append is skipped and its changes counted (`reason="newer_log"`) and logged.

On stop, the publisher finishes the append under way, appends what is still queued, and ends; the stop deadline aborts it. Every change that does not reach the log is counted once in `oagw_proxy_cache_publish_failures_total` by reason and logged with its kind, tenant and ID. Writes before the change feed starts are neither published nor counted; other replicas see them when their entries expire.

**Watcher.** One task per replica, only when the cache is enabled. It tracks the log it follows and the sequence number of the last change applied (the baseline).

| Event | Action |
|---|---|
| The watch starts, is re-established (`Reset`) or fell behind (`Lagged`) | Read the log and take its ID and last sequence number as the baseline, then empty the cache. The log is read first: every change up to the baseline was written before the cache was emptied, so no load after the flush misses one. |
| `Changed` | Read the log. The read fails, or the log is unreadable or a version this build does not read: empty the cache (cause `log_unreadable`), baseline unknown. A read that takes longer than 5 s does the same and also breaks the watch, which then reconnects. Another log ID, or no baseline: empty the cache and follow the new log. Last sequence number not after the baseline: nothing to do. Oldest change after the baseline + 1: a gap; empty the cache and follow from the log's end. Otherwise apply each change after the baseline that another process made, in order, and move the baseline. |
| `Deleted`, `Expired`, or the log is gone | Empty the cache; baseline unknown. |
| The watch closes with an error that cannot be retried | Empty the cache and stop watching. Freshness falls back to the TTL. |
| The watch closes with a retryable error, or its stream ends | Reconnect after a jittered backoff, then act as for a started watch. The backoff follows the cluster SDK's default policy (1 s doubling to 30 s) and is kept across reconnects, so a flapping watch does not empty the cache every second; it resets to 1 s once a watch stays up for 30 s. |

**Rolling upgrades.** During an upgrade to a newer log format, an old replica cannot read the new log and empties its cache on every change. It leaves the newer log alone: its appends are skipped and counted (`reason="newer_log"`), so other replicas see its writes only via the TTL, and its health check reports `Degraded` with code `proxy_cache_newer_log`. A new replica replaces an older-format log once. A writer starts a new log only when the stored one is malformed or of an older format.

After a rollback, the old replicas still skip the newer log and stay `Degraded` (`proxy_cache_newer_log`). To recover, delete the cluster key `oagw/proxy-config-changes`: the next append starts a new log, and every replica empties its cache once (`log_reset`). An old replica never replaces a newer log by itself, even after a quiet period: every newer format would have to accept replacement by an older one, and a quiet rolling upgrade would empty every replica's cache.

The watcher reconnects itself instead of using the SDK's `auto_restart`, which hides a break until the reconnect succeeds. While connecting or reconnecting, health reports `Degraded` with code `proxy_cache_watch_connecting`. A failed first watch (for example, a remote cluster service not up yet) is retried with the same backoff; `start` does not wait for it. Every log read is bounded by 5 s and ends at once when OAGW stops.

**Consistency.** OAGW does not require a `Linearizable` cache, which would rule out multi-node Redis. On an eventually consistent backend two concurrent appends can both succeed and one change can be lost; the affected entries on other replicas stay stale for at most the TTL.

### Cluster Setup

OAGW requires the cluster gear only when a database is configured and the cache is enabled:

* **With a database**, OAGW appends to and watches the change log through it. The cluster gear and an `oagw` profile are required.
* **With a database and `proxy_cache.enabled: false`**, there is no cache, no change log and no cluster requirement: every request reads the database, and writes append nothing. Set `enabled` alike on every replica: a replica with the cache off does not publish its writes, so replicas that cache see them only when their entries expire (the TTL).
* **Without a database**, OAGW neither uses nor requires the cluster gear. There is no cache (the in-memory lookup is already a few map lookups). This mode supports one replica only: each replica's configuration lives in its own memory, so writes never reach the others, and a change log cannot fix that. Use a database to run several replicas.

**How OAGW uses the cluster gear.** OAGW depends on `cluster-sdk` only, through its `cluster` feature, and does not declare `deps = [cluster]`. With a database and the cache enabled, it resolves the cache facade for profile `oagw` in `start` and requires the `Watch` capability (not `Linearizable`; see [Consistency](#the-change-log)).

**Builds.** The cluster gear runs inside the OAGW process (embedded) or as its own service (remote). The OAGW code is the same; only what the binary links changes, per the cluster design (`gears/system/cluster/docs/DESIGN.md`, §3.17.7), as in quota-enforcement:

| Build | Where the cluster gear runs | What the binary links | OAGW feature |
|---|---|---|---|
| Embedded | In the OAGW process | The `cluster` gear (it brings the standalone, PostgreSQL and Redis backends, and turns on `cluster-sdk/grpc-client` itself) and `grpc-hub`, which the cluster gear requires | `cluster`, which links `cluster-sdk` |
| Remote | As its own service, in another pod | Nothing extra | `grpc-client`, which turns on the `cluster` feature and `cluster-sdk/grpc-client` so the SDK reaches the cluster service over gRPC |

* The host binary chooses the build with its own features. The example server's `oagw-embedded = ["oagw", "api_egress/cluster", "dep:cluster", "grpc-hub"]` is the embedded build.
* Without either feature, OAGW links no cluster SDK and caches nothing: with a database and the cache enabled (the default), `init` fails; with `proxy_cache.enabled: false` it runs on the database. In-memory binaries (mini-chat, the example server's `oagw`) thus link no cluster code, so the SDK's proxy wiring, which fails startup without a cluster gear once a workspace build unifies `grpc-client` in, never runs there.
* OAGW does not link the cluster gear itself: which gears share a process is the binary's choice, and Cargo features cannot remove code.
* Turn on `grpc-client` only when the cluster service runs in another pod. The SDK derives its address from `POD_NAMESPACE`; where that is unset and no cluster gear runs, startup fails, with or without a database.
* The `cluster` gear turns on `cluster-sdk/grpc-client` itself, since it serves the cluster gRPC contract. Cargo unifies features per invocation, so a binary linking `cluster-sdk` built together with one linking the cluster gear (for example `cargo run --bin …` without `-p`) gets `grpc-client` too, and the previous point applies to it.
* Neither build is faster on the proxy path, which never calls the cluster gear. The remote build adds one network hop to each append and to the watch.

**Startup.** The cluster setup runs only with a database (`select_storage`, from `ctx.db()`) and the cache enabled, in `start` before the registry reconcile starts, so the reconcile's writes reach other replicas. The cluster SDK checks only resolved profiles:

* Embedded build: a missing `oagw` profile, or a backend without `Watch`, fails startup.
* Remote build: an unreachable cluster service does not block startup. The watch connects when the service is back; until then health reports `Degraded` and freshness falls back to the TTL. A missing profile or `Watch` is reported by the cluster SDK's readiness check once the service answers.
* Cluster gear not linked and `grpc-client` off: the SDK cannot tell this from a remote service still starting. After a 30 s grace period its readiness check reports `Unhealthy` with code `cluster-misconfigured`, keeping the pod out of rotation. The process does not exit, by the SDK's design.
* Without a database, or with the cache disabled: OAGW resolves nothing and starts ready with no cluster gear linked and no `oagw` profile.

OAGW's health check includes `cluster_sdk::cluster_readiness(ctx.client_hub())`, since the SDK cannot add its readiness check by itself.

**Redis.** For Redis, configure the `oagw` profile like this:

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

* `watch_mode: publish` (the default) is required. With `watch_mode: disabled` there is no watch, and OAGW fails at startup.
* A Redis cache is `EventuallyConsistent`, so the profile does not start without the `lock` (bound to `redis`) and `leader_election` (`allow_weak_consistency: true`) lines, though OAGW uses neither.
* Use `maxmemory-policy noeviction`, so Redis never evicts the change log key.
* Any topology works (single node, Sentinel, Redis Cluster); beyond a single node a concurrent append can be lost (see [Consistency](#the-change-log)).

**Other backends.** Any backend with exact watch and compare-and-swap works the same way (for example PostgreSQL or k8s). The standalone backend lives in one process and needs no infrastructure, so it fits a single replica only: with several, changes never reach other replicas. OAGW does not detect this misuse (watches are up, health stays healthy, writes show via the TTL only). A possible check is a startup warning when the `oagw` cache provider (`CacheDescriptor::provider`) is `standalone` and the database is shareable (PostgreSQL, MySQL); certain detection would need replicas to see each other, for example via a heartbeat row.

### Failure Handling

| Failure | Behavior |
|---|---|
| Database down | Cache hits are served. A miss returns the database error, as today. Errors are not cached; the next miss tries again. |
| A cache load hangs | After 30 s the load is dropped and every request waiting on it gets an error; the next miss starts a new load. |
| Appending to the change log fails, or takes longer than 2 s | The write still succeeds and local entries are evicted. OAGW logs a warning naming the change. A change whose append fails for good is counted (`reason="append_failed"`); one still in flight when the write stops waiting is not. Other replicas pick it up within the TTL. |
| A cluster call hangs | Each append attempt is cut after 1 s and retried; the append fails after about 5 s. A log read is cut after 5 s: the cache is emptied and the watch reconnects. |
| A newer build's log, during a rolling upgrade | An old replica skips its appends (`reason="newer_log"`) and reports `Degraded` (`proxy_cache_newer_log`); other replicas see its writes within the TTL. After a rollback, delete the log key ([Rolling upgrades](#the-change-log)). |
| OAGW stops | Queued changes are appended before the stop deadline. Changes it cannot append, or that arrive after it stopped, are counted (`reason="publisher_stopped"`) and logged. |
| A replica misses changes: a gap in the log, a new log | The replica empties its cache and follows the log from its current end. |
| The log cannot be read (unreadable, another version, or a failed `get`), or is deleted or expired | The replica empties its cache and has no baseline; the next log it reads empties the cache once more and becomes the baseline. |
| A concurrent append is lost on an eventually consistent backend | The affected entries on other replicas stay stale for at most the TTL. |
| The watch breaks | Proxying continues. The watcher reconnects with growing backoff; each reconnect reads the log and empties the cache. Meanwhile freshness falls back to the TTL and health reports `Degraded` (`proxy_cache_watch_connecting`). |
| The watch closes with an error that cannot be retried | The cache is emptied once and the watch stays down: freshness falls back to the TTL, and health reports `Degraded` (`proxy_cache_watch_down`). |
| With a database and the cache enabled, the `oagw` profile is not configured, or its backend has no `Watch` | Embedded build: startup fails. Remote build: the cluster SDK's readiness check reports it. |
| The cluster service cannot be reached at startup (remote build) | OAGW starts, reports `Degraded` (watch connecting, and the SDK's `cluster-starting`), and freshness falls back to the TTL until the watch connects. |
| The `oagw` profile's backend fails its probe (`cluster-profile-degraded`) | Proxying continues. The health check reports `Degraded`; freshness falls back to the TTL while the change log cannot be used. |

### Configuration

```yaml
gears:
  oagw:
    config:
      proxy_cache:
        enabled: true      # default true; no effect without a database
        ttl: 60s           # how long an entry may live; 1s to 1h
        max_entries: 10000 # per section; 1 to 1000000
```

* `ttl` is a humantime duration. Unknown keys are rejected.
* Without a database, the cache is off and no cluster configuration is needed.
* With a database and `enabled: true`, the cluster gear must have an `oagw` profile ([Cluster Setup](#cluster-setup)). With `enabled: false` there is no change log and no cluster is needed; set it alike on every replica.
* A build without the `cluster` feature cannot cache: with a database and `enabled: true`, `init` fails with an error naming the feature and `enabled: false`.
* The log bounds (128 changes, 32 KiB), the append attempts (5) and each attempt's limit (1 s), the write's wait for its append (2 s), the second apply of a dropped write (30 s later), a cache load's limit (30 s), a log read's limit (5 s), the health check's wait for the cluster readiness check (250 ms) and how long it reuses that check's last result (10 s) are constants, not configuration.

### Health and Metrics

**Health.** OAGW's health check is the worst of its parts, in this order:

* the change log watch, only with a database and the cache enabled: `Degraded` with code `proxy_cache_watch_connecting` while connecting or reconnecting, `proxy_cache_watch_down` once down (a non-retryable close, or after `stop`), and `proxy_cache_newer_log` while the watch is up but this replica's appends skip a newer-format log. A down watch never makes OAGW `Unhealthy`: the cache stays correct within the TTL;
* the cluster SDK's readiness (`cluster_sdk::cluster_readiness`), only with a database and the cache enabled. `cluster-starting` and `cluster-profile-degraded` make OAGW `Degraded` (not `Unhealthy` as the SDK reports them), since OAGW needs the cluster only for freshness; `cluster-misconfigured` makes it `Unhealthy`. The SDK's check can be slow, so OAGW runs it on its own task and waits at most 250 ms (half the API gateway's default 500 ms per check). A running check is left to finish and shared by later health checks; meanwhile OAGW reports the last result for up to 10 s after the check began, then `Degraded` with code `cluster_readiness_slow`. A gateway per-check timeout below 250 ms still times OAGW out;
* registry provisioning, as before.

The status and code are the worst part's (the earlier part's on a tie); the message names every unhealthy part.

**Metrics.** The lookup time is recorded as `oagw_request_duration_seconds{phase="resolve"}`, cache on or off, for comparison with the baseline. The cache adds:

| Metric | Type | Labels |
|---|---|---|
| `oagw_proxy_cache_lookups_total` | counter | `map` (`alias`, `route`), `result` (`hit`, `miss`, `joined`: waited for a load another request started) |
| `oagw_proxy_cache_entries` | gauge | `map` |
| `oagw_proxy_cache_evictions_total` | counter | `map`, `reason` (`change`, including emptying the whole cache; `ttl`; `capacity`) |
| `oagw_proxy_cache_changes_total` | counter | `source` (`local`: a write on this replica; `remote`: another replica's, from the log) |
| `oagw_proxy_cache_flushes_total` | counter | `cause` (`watch_reset`: the watch started, was re-established or fell behind; `log_gap`; `log_reset`: another log, or the log deleted, expired or gone; `log_unreadable`; `watch_closed`) |
| `oagw_proxy_cache_loads_discarded_total` | counter | `map` (loads not stored because a change detached them) |
| `oagw_proxy_cache_publish_failures_total` | counter | `reason` (`append_failed`: every attempt conflicted, failed or timed out, or the cluster refused it; `newer_log`: a newer build's log was left alone; `publisher_stopped`: the publisher had stopped, or stopped before the append). Each change not appended is counted once; a write that stops waiting after 2 s is not counted |

### Out of Scope

* **The tenant chain and the AuthZ check.** Both still run on every request. Caching the chain needs tenant hierarchy change events, which OAGW does not get.
* **Rate-limit counters.** They stay per replica ([ADR: Rate Limiting](./0004-rate-limiting.md)). Sharing them through the cluster gear is a separate decision.
* **Cross-replica cleanup of endpoint pools and rate-limit keys.** Endpoint pools already rebuild when the endpoints change ([ADR-0018, Endpoint Pool Freshness Across Replicas](./0018-optional-persistence.md#endpoint-pool-freshness-across-replicas)). Rate-limit keys of deleted upstreams and routes expire through the idle sweep.
* **The SDK's `resolve_proxy_target` with tags.** It reads the database as before.
* **A smarter capacity policy.** Only the choice of which entry goes when a section is full would change (for example TinyLFU, behind the full-key map).

### Relation to Earlier ADRs

* Supersedes [ADR: Control Plane Caching](./0007-data-plane-caching.md). Its design did not handle these cases:
  * Its keys (`upstream:{tenant_id}:{alias}`) cannot follow inheritance from ancestor tenants.
  * It had no TTL, so one lost invalidation leaves a replica stale forever.
  * It stored configuration in Redis, which costs about as much as the database read it replaces.
  * It left cross-replica invalidation unspecified.
* Replaces the Data Plane L1 cache in [ADR: State Management](./0008-state-management.md). OAGW runs both planes in one process, so one cache is enough. The rate limiter part of ADR-0008 stays.
* Builds on [ADR: Optional Persistence](./0018-optional-persistence.md). Its rules for the database, transactions and endpoint pools do not change.

### Expected Gains

These are estimates from the query count and typical round-trip times, not measurements. The [benchmark](#benchmark-plan) checks them before this ADR is accepted.

**Latency per lookup.**

| | Today | Cache hit |
|---|---|---|
| Queries | 3 `SELECT`s in a row (5 when the winning route changes between Q2 and Q3) | none (a miss: 1 or 2) |
| PostgreSQL in the same zone | about 1–3 ms (0.3–1 ms per query) | a few µs |
| Embedded SQLite | about 30–100 µs | a few µs |

* On PostgreSQL a hit saves about 1–3 ms, 10–30% of the 10 ms p95 overhead budget (`cpt-cf-oagw-nfr-low-latency`).
* The larger gain is at p99: under load the queries wait for a pool connection and the database, pushing p99 to tens of milliseconds. A hit uses no connection.
* The tenant resolver, AuthZ, upstream selection, SSRF check, `select_route` and merge still run; a networked tenant resolver becomes the largest remaining cost.
* End to end the saving is small next to an upstream LLM call; it matters for OAGW's own overhead and tail latency.

**Database load.**

* Today: queries per second = 3 × requests per second. Load grows with traffic.
* With the cache: queries per second ≈ replicas × active keys ÷ TTL, plus, on each replica, one reload of each entry a write affects. Load grows with the number of active keys and the writes, not with traffic.

Example: 3 replicas, 1,000 requests per second in total, 500 active alias keys, 300 upstreams with routes, TTL 60 s.

| | Queries per second |
|---|---|
| Today | 3 × 1,000 = 3,000 |
| With the cache, no writes | 3 × (500 + 300) ÷ 60 ≈ 40 (about 98% fewer) |
| Each write | one reload of each affected entry on each replica, as the keys are requested again: a few for a write in one customer tenant; up to every alias entry holding the upstream for a write to a root upstream that all tenants inherit |

**Where the gain is smaller.**

* **Quiet tenants.** Each tenant has its own alias entry (the key holds the chain), even when all resolve to the same root upstream. A tenant requesting every few minutes finds its entry expired and runs Q1; the shared route section still saves Q2 and Q3.
* **Frequent writes.** A write to an upstream many tenants inherit reloads every entry holding it. A replica more than 128 changes behind empties its cache once.
* **A single replica on SQLite.** The queries are already in-process; the gain is mostly less CPU and locking.

### Benchmark Plan

The benchmark runs before this ADR is accepted. If it does not show a clear gain on PostgreSQL, the "No cache" option is reconsidered.

* **What runs.** `resolve_proxy_target` without tags, with the cache off and on. PostgreSQL in a test container, and SQLite.
* **Data.** A three-level tenant hierarchy (root, 10 partners, 1,000 customers). Upstreams in each level, with shadowing and inheritance. About 10 routes per upstream.
* **Loads.**
  * Hot: 20 alias keys, many requests each.
  * Quiet tenants: each request from a different customer.
  * Writes: the hot load plus one write per second in other tenants. The hit ratio is expected to stay near the no-write case; with a full flush on every write it would fall.
* **Concurrency.** 1, 16 and 64 concurrent lookups, with the default pool size.
* **Reported.** p50 and p99 lookup time, queries per lookup, and the hit ratio. Queries are counted the same way as in the existing query-count tests.
* **Where.** Behind the `integration` feature and not run in CI by default. The results are written into this ADR.

Production lookups are compared through the `resolve` phase and the cache metrics ([Health and Metrics](#health-and-metrics)).

### Consequences

* A cache hit runs no database query. The database load from proxying drops from three `SELECT`s per request to about one load per active key, per replica, per TTL, plus one reload of each entry a write affects.
* Proxying keeps working for cached aliases while the database is slow or down.
* With several replicas, another replica's write can take up to the TTL to show when a change is lost.
* Every deployment with a database and the cache enabled needs the cluster gear and an `oagw` profile whose cache supports watches (a single replica can use the infrastructure-free standalone backend). With the cache disabled it needs neither, and every request reads the database. A deployment without a database needs neither, and supports one replica only.
* The proxy reads through a new port, `ProxyConfigReader`, given to `ControlPlaneServiceImpl` when the cache is enabled. On a database, repository decorators evict and publish on every write. `RouteRepository` gets `list_proxy_routes` ([ADR-0009 Appendix B](./0009-storage-schema.md#appendix-b-queries-and-access-frequency) Q15). `RepositoryError` becomes `Clone`, so waiters on one load share its error.
* OAGW gets an optional `cluster-sdk` dependency (feature `cluster`) and a `grpc-client` feature for the remote build. Binaries running OAGW with a database also link the cluster gear and `grpc-hub`, or turn on `grpc-client`. OAGW's database tests use the standalone backend, and Redis under `integration`.
* The query-count tests for the proxy lookup run with the cache off. New tests count queries on a hit and on a miss.
* DESIGN §4.1 and §4.2, DECOMPOSITION, the proxy engine, management API and observability features, the OAGW README (deployment), ADR-0007, ADR-0008 and `scenarios/flows/cache-invalidation.md` describe this cache. [ADR-0009 Appendix B](./0009-storage-schema.md#appendix-b-queries-and-access-frequency) and `docs/migration.sql` give the query frequencies with the cache on and off.

### Scenarios

Each scenario lists what happens and the test IDs that check it (see [Confirmation](#confirmation)).

1. **Repeated request.** The first request for an alias loads it from the database. The next requests for the same alias and tenant chain run no `SELECT`. (`CACHE-HIT-001`, `CACHE-HIT-002`)
2. **Unknown alias.** A request for an alias that does not exist returns "not found". Repeating it runs no `SELECT`. Creating the alias makes the next request find it. (`CACHE-HIT-003`, `CACHE-EVICT-001`, `CACHE-FRESH-001`)
3. **Change on the same replica.** An upstream or route is created, updated, disabled or deleted through the Management API, the SDK or the registry reconcile, or the write's request is cancelled mid-call. The next request on the same replica sees the change. (`CACHE-FRESH-001`, `CACHE-FRESH-002`)
4. **Upstream written.** An upstream is created in a tenant: the entries for its alias whose chain holds the tenant are evicted, "not found" included. An upstream is updated, disabled, shared differently or renamed: the entries holding it are evicted, and on a rename the entries for the new alias too. An upstream is deleted: the entries holding it and its route entry are evicted. Other entries stay. (`CACHE-EVICT-001`, `CACHE-EVICT-002`, `CACHE-EVICT-003`)
5. **Route written.** A route is created, updated or deleted: its upstream's route entry is evicted. A route moves to another upstream: both route entries are evicted. (`CACHE-EVICT-004`)
6. **Write in another tenant.** A write in one tenant leaves the other tenants' entries cached; their next requests run no `SELECT`. (`CACHE-EVICT-005`)
7. **Eviction rules are complete.** Random writes across a three-level hierarchy, interleaved with lookups: after every step each cached lookup equals the uncached one, on one replica and across two replicas once the change is delivered. (`CACHE-EVICT-007`)
8. **Change on another replica.** Replica A writes and appends the change to the log. Replica B reads the log and evicts the same entries. B's next request sees the change. (`CACHE-FRESH-003`, `CACHE-LOG-001`)
9. **Change in an ancestor tenant.** A parent tenant creates, shares, disables or deletes an upstream with the alias a child tenant uses. The child's next request resolves the new result. (`CACHE-FRESH-004`)
10. **Upstream deleted with its routes.** The database removes the routes by cascade. The next request returns "not found". (`CACHE-FRESH-005`)
11. **Tenant moved.** A tenant moves under another parent. The next request uses the new tenant chain and resolves through the new ancestors. (`CACHE-FRESH-006`)
12. **Change lost.** Replica B never hears of a change. B serves the old configuration until the TTL expires, then reads the new one. (`CACHE-FRESH-007`)
13. **Watch dropped events, reconnected, or the log was removed.** The watch reports `Lagged`, `Reset`, `Deleted` or `Expired`. The replica empties its cache. (`CACHE-FRESH-008`)
14. **Changes missed in the log.** The log has a gap after the replica's last change, or has a new ID: the replica empties its cache and follows the log from its end. The log cannot be read (for example a newer format, or the read fails): the replica empties its cache and has no baseline, so the next log it reads empties the cache once more and becomes the baseline. (`CACHE-LOG-002`)
15. **Concurrent and bursty writes.** Two replicas append at the same time: both changes land. A burst of writes on one replica is one append. The log stays within 128 changes and 32 KiB. (`CACHE-LOG-003`)
16. **Read races a write.** A request starts loading the old configuration, a write's change detaches the load, then the load finishes. The requests waiting on it get its result, it is not stored, and a later request starts a new load. (`CACHE-RACE-001`, `CACHE-EVICT-006`)
17. **Many misses at once.** Many requests miss the same key at the same time. One `SELECT` runs, and all requests get its result. (`CACHE-RACE-002`)
18. **Host denied after caching.** The SSRF policy starts denying a cached upstream's host. The next request is rejected. (`CACHE-SAFE-001`)
19. **Database down.** Cached aliases keep resolving. A miss returns the database error, and the error is not cached. (`CACHE-FAIL-001`)
20. **Change cannot be appended.** The write succeeds and the local entries are evicted. Each change not appended is counted once, by reason, and logged with its IDs; changes queued when OAGW stops are still appended. Other replicas see the change within the TTL. (`CACHE-FAIL-002`)
21. **Watch down.** Proxying continues, the health check reports `Degraded` (`proxy_cache_watch_connecting` while reconnecting, `proxy_cache_watch_down` once closed), and freshness falls back to the TTL. A log read that hangs breaks the watch. Quick reconnects back off. After reconnecting, the cache is emptied and health recovers. (`CACHE-FAIL-003`)
22. **Cluster profile missing.** With a database, the cache enabled and no `oagw` profile, an embedded build fails startup. (`CACHE-CONF-001`)
23. **No database, or cache disabled.** Without a database, or with `enabled: false`, every request reads the repositories as before. With a database and `enabled: false`, writes append nothing, and OAGW starts and is ready with no cluster. (`CACHE-CONF-002`)
24. **Lookup with tags.** The SDK's `resolve_proxy_target` with tags reads the database every time. (`CACHE-CONF-003`)
25. **Cache full.** The least recently used entry is evicted, and requests still resolve correctly. (`CACHE-HIT-004`)
26. **Cluster service unreachable at startup.** In the remote build, OAGW starts, reports `Degraded` (the watch connecting; the cluster SDK's `cluster-starting` is `Degraded` too, not `Unhealthy`), and recovers when the watch connects. A misconfiguration the SDK reports after its grace period makes OAGW `Unhealthy`. (`CACHE-CONF-004`)
27. **No database, no cluster.** Without a database, OAGW starts and is ready with no cluster gear linked and no `oagw` profile. (`CACHE-CONF-005`)

### Confirmation

Each behavior below has an ID. The tests that prove it name the ID in their doc comment (`/// ADR-0020: CACHE-HIT-001.`), as in ADR-0018. "Every database" means SQLite, PostgreSQL and MySQL. Two replicas are two `ControlPlaneServiceImpl` instances over one database and one shared cluster cache. The Redis tests run with the `integration` feature.

**Cache hits**

* `CACHE-HIT-001`: a second identical proxy lookup runs no `SELECT`, on every database.
* `CACHE-HIT-002`: a hit returns the same upstream, route and effective configuration as a lookup without the cache, over a multi-level tenant hierarchy with shadowing and inheritance.
* `CACHE-HIT-003`: "not found" and "disabled" are cached: repeating the request runs no `SELECT` and returns the same error.
* `CACHE-HIT-004`: when a section is full, the least recently used entry is evicted and lookups stay correct.

**Eviction**

* `CACHE-EVICT-001`: an upstream create in tenant `T` evicts the entries for its alias whose chain contains `T`, "not found" entries included, and nothing else.
* `CACHE-EVICT-002`: an upstream update, disable or sharing change evicts the entries holding it; an alias rename evicts the entries for the old and the new alias.
* `CACHE-EVICT-003`: an upstream delete, through the API and through `delete_upstream_if_unused`, evicts the entries holding it and its route entry.
* `CACHE-EVICT-004`: a route create, update or delete evicts its upstream's route entry; a move evicts both upstreams' entries.
* `CACHE-EVICT-005`: a write in one tenant leaves other tenants' entries cached: their next lookups run no `SELECT`.
* `CACHE-EVICT-006`: a change detaches the in-flight loads of its section: their waiters get the result, it is not stored, and a later request starts a new load.
* `CACHE-EVICT-007`: a seeded randomized model test runs random writes across a three-level hierarchy (upstream create, update, disable, share, rename, delete; route create, update, move, delete) interleaved with lookups; after every step each cached lookup equals the uncached one. On one service, and on two services over one database and one cluster after the change is delivered.

**Freshness**

* `CACHE-FRESH-001`: on one replica, each write (upstream and route create, update, disable, delete) is visible to the next lookup.
* `CACHE-FRESH-002`: each write path evicts: Management API, SDK, registry reconcile, and `delete_upstream_if_unused`, including a write whose future is dropped mid-call, which applies and publishes its change once more 30 s later.
* `CACHE-FRESH-003`: with two replicas and a shared cluster cache, a write on one is visible to the next lookup on the other after the change is delivered. Also against a real Redis.
* `CACHE-FRESH-004`: a write in an ancestor tenant (create, share, disable, delete) changes the next lookup of a child tenant.
* `CACHE-FRESH-005`: deleting an upstream makes its alias "not found" and its routes unmatched.
* `CACHE-FRESH-006`: a changed tenant chain uses a new key and resolves through the new ancestors.
* `CACHE-FRESH-007`: with a change lost, the other replica sees the write after the TTL and not before.
* `CACHE-FRESH-008`: `Lagged`, `Reset`, `Deleted` and `Expired` from the watch empty the cache.

**Change log**

* `CACHE-LOG-001`: changes from another replica are applied in order; a replica's own changes are skipped.
* `CACHE-LOG-002`: a gap or a new log ID empties the cache, and the replica follows the log from its end; an unreadable or newer-version log empties the cache and leaves no baseline, so the next log read empties it once more and becomes the baseline. A writer replaces a malformed or older-format log and leaves a newer-format one alone, and reports `Degraded` (`proxy_cache_newer_log`) until one of its appends lands.
* `CACHE-LOG-003`: concurrent appends from two replicas both land (compare-and-swap retry, with a backoff from 20 ms doubling); a burst of changes is one append, and the writes of a registry reconcile run wait once, at its end; the log stays within 128 changes and 32 KiB.

**Races**

* `CACHE-RACE-001`: a load whose database read started before a write, and finished after the write's change was applied, does not store its result (the change detached it).
* `CACHE-RACE-002`: concurrent misses on one key run one load.

**Safety**

* `CACHE-SAFE-001`: a cached upstream whose host the SSRF policy later denies is rejected on the next lookup.

**Failures**

* `CACHE-FAIL-001`: with the database unavailable, cached lookups succeed, a miss returns the error, and the next lookup tries the database again.
* `CACHE-FAIL-002`: when appending the change fails, the write succeeds, the local entries are evicted, and each change not appended is logged with its IDs and counted once in `oagw_proxy_cache_publish_failures_total` by `reason`; a hung cluster call fails its attempt after 1 s; on stop the queued changes are still appended, and those the publisher cannot take are counted.
* `CACHE-FAIL-003`: when the watch breaks, or a log read hangs for 5 s, the health check reports `Degraded` with code `proxy_cache_watch_connecting`; after it reconnects, the cache is emptied and the health check recovers; quick reconnects back off from 1 s doubling.

**Configuration**

* `CACHE-CONF-001`: with a database and the cache enabled, and without an `oagw` cluster profile or with a backend that has no `Watch`, an embedded build fails startup.
* `CACHE-CONF-002`: without a database, or with `enabled: false`, every lookup reads the repositories; with a database and `enabled: false`, OAGW starts and reports ready with no cluster client registered, and its writes append nothing.
* `CACHE-CONF-003`: `resolve_proxy_target` with tags reads the database every time.
* `CACHE-CONF-004`: with the cluster service unreachable at startup (remote build), OAGW starts and reports `Degraded`; once the watch connects, the cache is emptied and the health check recovers. The cluster SDK's `cluster-starting` and `cluster-profile-degraded` make OAGW `Degraded`, `cluster-misconfigured` `Unhealthy`. A cluster readiness check slower than 250 ms does not hold up OAGW's health check: it reports the last result for up to 10 s, then `Degraded` with code `cluster_readiness_slow`. On a tie, the watch's code shows before provisioning's.
* `CACHE-CONF-005`: without a database, OAGW starts and reports ready with no cluster client registered, and never resolves the `oagw` profile.

## Open Questions

These choices are proposed defaults, to confirm in review:

* The TTL default: 60 s.
* Applying a write's change after every write call, including failed ones, instead of only after successful commits.
* Keying the alias section by alias and tenant chain. It needs no eviction when a tenant moves, but quiet tenants miss often (see [Expected Gains](#expected-gains)).

Answered:

* Emptying the whole cache on every change: no, only the affected entries; the whole cache is emptied only when a replica cannot know what it missed ([The Change Log](#the-change-log)).
* Whether a write waits for its change to reach the log: yes, at most 2 s; a registry reconcile run waits once, at its end ([Keeping the Cache Fresh](#keeping-the-cache-fresh)).
* Whether a late commit of a dropped write can leave stale entries: its change is applied and published again 30 s after the drop; later completions are covered by the TTL.
* Whether an older build replaces a newer-format log after a quiet period, so a rollback recovers by itself: no; the operator deletes the log key ([Rolling upgrades](#the-change-log)).
* Whether a cluster outage takes OAGW out of rotation: no, only `cluster-misconfigured` makes it `Unhealthy` ([Health and Metrics](#health-and-metrics)).
* Whether requests waiting on one load share its error: yes. Errors are still never stored.
* Whether the `oagw` profile must be `Linearizable`: no ([Consistency](#the-change-log)).
* The log's size: 128 changes and 32 KiB.

## Pros and Cons of the Options

### In-memory cache, cluster change log, targeted eviction, TTL backstop

* Good, because a hit runs no query and other replicas see changes within milliseconds
* Good, because a write reloads only the entries it affects, so the hit ratio does not depend on writes being rare
* Good, because the database stays the only store of configuration
* Good, because the cluster backend is chosen by configuration (Redis, PostgreSQL, k8s)
* Good, because the TTL bounds staleness when a change is lost, and a replica that cannot know what it missed empties its cache
* Bad, because the eviction rules must be complete: a missed case leaves a stale entry until the TTL. A randomized model test (`CACHE-EVICT-007`) checks them against the uncached lookup
* Bad, because every deployment with a database and the cache enabled needs the cluster gear, and several replicas need a shared backend (Redis, PostgreSQL, k8s)
* Bad, because a change appended after the commit can be lost, so staleness up to the TTL is possible

### In-memory cache, cluster change signal, full flush

Every write empties the writer's whole cache and writes a new random value to one cluster key; every replica watches the key and empties its whole cache. A counter that goes up on each flush keeps a load that raced a flush from being stored.

* Good, because it is simple: one rule for every write, and the signal carries no data
* Good, because other replicas see changes within milliseconds
* Bad, because every write, in any tenant, empties every replica's cache. The write rate is unknown, and with many tenants writing, the hit ratio would stay low
* Bad, because each write reloads every active key on every replica

### In-memory cache, database generation poll

Each write increases a counter row in the same transaction. Each replica reads the counter every second or so.

* Good, because a change cannot be lost: the counter is written with the change
* Good, because it needs no new dependency
* Bad, because other replicas see changes only after the next poll
* Bad, because every replica queries the database even when nothing changes
* Bad, because a counter says only that something changed, so every change empties the whole cache

### In-memory cache, TTL only

* Good, because it is the simplest
* Bad, because other replicas see changes only after the TTL, so the TTL must be short, which lowers the hit rate

### Configuration in a shared cache

Store the configuration in Redis as a second cache level (ADR-0007).

* Good, because replicas share one copy
* Bad, because a Redis read costs about as much as the database read it replaces
* Bad, because the domain types need serialization, and values and keys must fit the shared cache's limits
* Bad, because it still needs a per-replica cache and invalidation to remove I/O from the proxy path

### No cache

* Good, because it needs no change
* Bad, because every request runs three `SELECT`s and fails when the database is unavailable

## More Information

- [ADR: Optional Persistence](./0018-optional-persistence.md) — the database repositories and the proxy lookup this ADR caches
- [ADR: Control Plane Caching](./0007-data-plane-caching.md) — superseded by this ADR
- [ADR: State Management](./0008-state-management.md) — its Data Plane L1 cache is replaced by this ADR
- [ADR: Rate Limiting](./0004-rate-limiting.md) — rate-limit counters, not changed by this ADR
- Cluster gear: `gears/system/cluster/docs/DESIGN.md` — cache watch semantics (`Lagged`, `Reset`, at-most-once delivery), compare-and-swap, consistency levels

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-nfr-low-latency` — A cache hit runs no database query on the proxy path
* `cpt-cf-oagw-nfr-high-availability` — Cached aliases keep resolving while the database is unavailable
* `cpt-cf-oagw-fr-alias-resolution` — Alias resolution over the tenant chain, served from the cache with the same results
* `cpt-cf-oagw-fr-hierarchical-config` — A write in an ancestor tenant evicts every entry whose tenant chain contains it, so descendant lookups see it
* `cpt-cf-oagw-fr-config-persistence` — The database stays the only store of configuration
