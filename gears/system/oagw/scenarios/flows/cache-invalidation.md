# Cache Invalidation Flow

<!-- toc -->

- [Overview](#overview)
- [Cache Architecture](#cache-architecture)
  - [Sections](#sections)
  - [What a Write Evicts](#what-a-write-evicts)
- [Invalidation Trigger: Config Write](#invalidation-trigger-config-write)
- [Scenario 1: Update Upstream Config](#scenario-1-update-upstream-config)
  - [Initial State](#initial-state)
  - [Management Request](#management-request)
  - [Invalidation Flow](#invalidation-flow)
  - [Post-Invalidation State](#post-invalidation-state)
  - [Next Proxy Request](#next-proxy-request)
- [Scenario 2: Create New Route](#scenario-2-create-new-route)
- [Scenario 3: Delete Upstream (with cascade)](#scenario-3-delete-upstream-with-cascade)
- [Scenario 4: Upstream Created in an Ancestor Tenant](#scenario-4-upstream-created-in-an-ancestor-tenant)
- [Scenario 5: Multi-Instance Environment](#scenario-5-multi-instance-environment)
  - [Setup](#setup)
  - [Update Flow](#update-flow)
  - [Next Request to the Other Instance](#next-request-to-the-other-instance)
- [Scenario 6: Changes Missed](#scenario-6-changes-missed)
- [Invalidation Timing](#invalidation-timing)
- [Consistency Guarantees](#consistency-guarantees)
- [Failure Scenarios](#failure-scenarios)
  - [Change Log Append Fails](#change-log-append-fails)
  - [Change Log Watch Down](#change-log-watch-down)
  - [Database Write Fails](#database-write-fails)
  - [Read Races a Write](#read-races-a-write)
- [Monitoring](#monitoring)
- [Related ADRs](#related-adrs)

<!-- /toc -->

## Overview

With a database, each OAGW instance keeps a per-instance cache of the configuration the proxy reads ([ADR: Proxy Configuration Cache](../../docs/ADR/0020-proxy-config-cache.md)). A configuration write evicts only the entries it can affect: on the writing instance before the write answers, and on every other instance through a short change log in the cluster gear. Every entry also expires after a TTL (default 60 s), which bounds staleness when a change is lost. When an instance cannot know what it missed, it empties its whole cache.

Without a database there is no configuration cache: the proxy reads the in-memory repositories directly, and OAGW supports one instance only.

## Cache Architecture

### Sections

| Section | Key | Value |
|---|---|---|
| Alias | normalized alias + tenant chain (requesting tenant first, then ancestors) | Every upstream with the alias owned by a tenant in the chain, disabled and not-visible ones included; empty when there is none ("not found") |
| Route | the upstream's tenant and ID | The upstream's enabled HTTP routes, in full (path, methods, plugins, priority, rate limit, CORS) |

- Both sections hold raw repository answers. Upstream selection, the SSRF check, route selection and the merge run on every request, so a hit returns what an uncached lookup returns.
- Each section indexes its entries by the IDs they hold (upstream IDs in the alias section, route IDs in the route section), and the alias section also by alias. A write finds the entries it affects through these indexes.
- Each entry expires after `proxy_cache.ttl`. Each section holds at most `proxy_cache.max_entries` (default 10,000); when full, the least recently used entry goes.
- The database is the only source of truth. The cluster gear holds only the change log, never configuration.

### What a Write Evicts

| Write | Alias section evicts | Route section evicts |
|---|---|---|
| Upstream `U` in tenant `T` created or updated, with alias `a` | entries holding `U`; entries for `a` whose chain contains `T` | — |
| Upstream `U` in `T` deleted | entries holding `U` | `U`'s entry |
| Route `R` on upstream `U` created or updated | — | `U`'s entry; entries holding `R` |
| Route `R` deleted | — | entries holding `R` |
| The routes of upstream `U` deleted | — | `U`'s entry |

A write also detaches the loads in flight in each section it touches, so a load that read rows from before the write is not stored.

## Invalidation Trigger: Config Write

The cache wraps the database repositories, so every upstream and route create, update and delete evicts, whether from the Management API, the SDK or the registry reconcile. The eviction runs after the call ends, succeeded, failed or cancelled mid-call. A write cancelled mid-call evicts and publishes its change once more 30 s later, since its statement can still commit after the cancel. A Management API or SDK write answers after its change is appended (at most 2 s); a registry reconcile run waits once, at its end.

## Scenario 1: Update Upstream Config

### Initial State

Tenant `T` (child of partner `P`, child of root `R`) has proxied requests to `api.openai.com`. Instance A's cache:

```text
alias section:
  (api.openai.com, [T, P, R]) → [upstream U1 (tenant T), upstream U0 (tenant R)]   holds U1, U0
  (api.openai.com, [T2, P, R]) → [upstream U0 (tenant R)]                          holds U0
route section:
  (T, U1) → [route R1, route R2]
  (R, U0) → [route R9]
```

### Management Request

```http
PUT /api/oagw/v1/upstreams/gts.cf.core.oagw.upstream.v1~<U1> HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <tenant-token>
Content-Type: application/json

{
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "api.openai.com", "port": 443 }
    ]
  },
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "enabled": false
}
```

### Invalidation Flow

```text
Instance A
┌──────────────────────────────────────────────┐
│ 1. Build the change from the call:           │
│    Upstream { tenant: T, id: U1,             │
│               alias: api.openai.com }        │
│ 2. Repository update commits                 │
│ 3. Evict locally:                            │
│    - entries holding U1                      │
│    - entries for api.openai.com whose chain  │
│      contains T                              │
│    → (api.openai.com, [T, P, R]) evicted     │
│    detach in-flight alias loads              │
│ 4. Append the change to the cluster log      │
│    oagw/proxy-config-changes (≤ 2 s wait)    │
│ 5. Return 200 OK                             │
└──────────────────────────────────────────────┘
```

### Post-Invalidation State

```text
alias section:
  (api.openai.com, [T2, P, R]) → [upstream U0]     kept: T2's chain does not contain T
route section:
  (T, U1) → [route R1, route R2]                   kept: an upstream write does not touch routes
  (R, U0) → [route R9]                             kept
```

### Next Proxy Request

```text
Tenant T: POST /api/oagw/v1/proxy/api.openai.com/v1/chat/completions
  → alias section (api.openai.com, [T, P, R]): MISS
  → SELECT the upstreams with the alias across [T, P, R]; store [U1 (disabled), U0]
  → selection: U1 is disabled, U0 is visible and enabled → U0 (if it is shared to T)
  → route section (R, U0): HIT
  → select_route, merge, proxy
Tenant T2: same alias
  → alias section (api.openai.com, [T2, P, R]): HIT, no SQL
```

## Scenario 2: Create New Route

```http
POST /api/oagw/v1/routes HTTP/1.1
Content-Type: application/json

{ "upstream_id": "gts.cf.core.oagw.upstream.v1~<U1>", "match": { "http": { "methods": ["POST"], "path": "/v1/embeddings" } } }
```

```text
1. Change: Route { tenant: T, id: R3, upstream: U1 }
2. Commit
3. Evict (T, U1) from the route section, and entries holding R3 (none: it is new);
   detach in-flight route loads. The alias section is untouched.
4. Append to the change log, return 201
Next request: alias HIT, route section (T, U1) MISS → one SELECT for U1's routes
```

A route moved to another upstream evicts both upstreams' route entries: the new upstream's by key, the old one's because it holds the route.

## Scenario 3: Delete Upstream (with cascade)

```http
DELETE /api/oagw/v1/upstreams/gts.cf.core.oagw.upstream.v1~<U1> HTTP/1.1
```

```text
1. The database deletes U1; the foreign-key cascade deletes its routes and their rows
2. Evict: alias entries holding U1, and the route entry (T, U1)
3. Append to the change log, return 204
Next request from T: alias MISS → SELECT returns [U0] (or [] → 404 "not found", cached)
```

The registry reconcile's `delete_upstream_if_unused` evicts the same entries.

## Scenario 4: Upstream Created in an Ancestor Tenant

Partner `P` creates an upstream with alias `api.example.com`, shared to its descendants. Tenants `T` and `T3` under `P` had cached "not found" for that alias; tenant `X` under another partner had too.

```text
Change: Upstream { tenant: P, id: U5, alias: api.example.com }
Evict: entries for api.example.com whose chain contains P
  → (api.example.com, [T, P, R])   evicted ("not found" entry)
  → (api.example.com, [T3, P, R])  evicted
  → (api.example.com, [X, Q, R])   kept: P is not in X's chain
```

The next request from `T` or `T3` loads the alias again and finds `U5`. A tenant that moves under another parent gets a new chain, so its next request uses a new key; no eviction is needed.

## Scenario 5: Multi-Instance Environment

### Setup

```text
Instance A ─┐                ┌─ PostgreSQL (configuration, source of truth)
Instance B ─┼── both read ───┤
            │                └─ cluster gear, profile oagw (Redis, PostgreSQL or k8s)
            └── both append to and watch oagw/proxy-config-changes
```

Each instance has its own cache. The standalone cluster backend cannot be shared between processes, so several instances need a shared backend.

### Update Flow

```text
Instance A                                   Cluster                         Instance B
1. commit, evict locally
2. read log {log: L, seq: 41}
3. compare-and-swap with change seq 42  ──→  log {log: L, seq: 42, ...}
4. return 200                                watch event: Changed  ──────→  5. read the log
                                                                            6. baseline (L, 41):
                                                                               apply seq 42 (origin A ≠ B)
                                                                               evict the same entries
                                                                            7. baseline (L, 42)
```

- A burst of writes on one instance, such as a registry reconcile run, becomes one append. Two instances appending at once conflict on the compare-and-swap; the loser retries (up to 5 attempts, each at most 1 s), so both changes land.
- An instance skips its own changes in the log: it already applied them.

### Next Request to the Other Instance

```text
Instance B: tenant T requests api.openai.com
  → alias section MISS (evicted at step 6) → SELECT → fresh result
```

## Scenario 6: Changes Missed

An instance that cannot know what it missed empties its whole cache. After a reconnect, a lag, a gap or a new log ID it follows the log from its current end. After an unreadable log (including a failed read) or a deleted or expired one it has no baseline: the next log it reads empties the cache once more and becomes the baseline.

| Event | Why |
|---|---|
| The watch starts, reconnects (`Reset`) or fell behind (`Lagged`) | Changes may have been made while it was not listening. The instance reads the log first, then empties the cache, so no later load misses a change up to the baseline. |
| The oldest change in the log is after the baseline + 1 | The log keeps the last 128 changes and 32 KiB; older ones were dropped before this instance read them. |
| The log has a new ID | The log was created again (deleted, or replaced because it was malformed or of an older format). |
| The log cannot be read, or is a newer format | During a rolling upgrade, an old instance empties its cache on every change until upgraded, and skips its own appends (`reason="newer_log"`, healthcheck code `proxy_cache_newer_log`), so the others see its writes within the TTL. A `get` that fails or takes longer than 5 s counts as unreadable (`cause="log_unreadable"`); a slow one also reconnects the watch. |
| The log is deleted or expired | Nothing to follow until the next append. |

## Invalidation Timing

| Step | Typical latency |
|---|---|
| Local eviction | < 1 ms (in memory, under the section lock) |
| Change log append | one cluster round trip (embedded: in process; remote: + one gRPC hop); a Management API or SDK write waits for it for at most 2 s, a registry reconcile run once at its end |
| Other instances evict | append + watch event + one cluster read: a few milliseconds |
| Lost change corrected | at most `proxy_cache.ttl` (default 60 s) |

## Consistency Guarantees

- **Own writes**: an instance sees its own write on the next lookup. The eviction happens before the write answers.
- **Other instances' writes**: seen once the change is delivered, usually within milliseconds. The write waits for its append, so a client that then calls another instance only races the watch delivery.
- **Lost changes**: bounded by the TTL. On an eventually consistent backend (Redis beyond a single node) two concurrent appends can both succeed and lose one change.
- **Results**: a hit returns the same upstream, route and errors as an uncached lookup. The SSRF check runs on every request, so a host denied after caching is rejected at once.

## Failure Scenarios

### Change Log Append Fails

```text
Instance A commits and evicts locally
  ↓
Append fails after 5 attempts (each at most 1 s), or does not finish in 2 s
  ↓
WARN logged; a change whose append fails for good counts once in
oagw_proxy_cache_publish_failures_total{reason="append_failed"}
(a timed-out wait alone is not counted)
  ↓
Returns success to client (the database is the source of truth)
```

**Impact**: other instances serve the old entries until they expire (at most the TTL).

The same counter also counts `reason="newer_log"` (an older build skipping its appends during a rolling upgrade) and `reason="publisher_stopped"` (changes still queued at the stop deadline).

### Change Log Watch Down

```text
Watch breaks (retryable), the stream ends, or a log read takes longer than 5 s
  ↓
Health: Degraded, code proxy_cache_watch_connecting; proxying continues
  ↓
Reconnect with backoff (1 s doubling to 30 s, kept across quick reconnects), read the log, empty the cache
  ↓
Health recovers
```

A watch that closes with an error that cannot be retried empties the cache once and stays down (code `proxy_cache_watch_down`); freshness then relies on the TTL.

The health check includes the cluster SDK's readiness: `cluster-starting` and `cluster-profile-degraded` report `Degraded` and proxying continues; only `cluster-misconfigured` reports `Unhealthy`.

### Database Write Fails

```text
Repository write: FAILURE
  ↓
The change is still evicted and appended (the write may have committed before it failed)
  ↓
Returns the error to the client
```

**Impact**: at most some extra reloads; caches never keep data the database no longer has.

### Read Races a Write

```text
Request 1: alias MISS → load starts, reads rows from before the write
Write:     commit → evict → detach the alias section's in-flight loads
Request 1: load returns → answers request 1 (it arrived before the write) → NOT stored
Request 2: MISS → new load → reads the committed rows → stored
```

## Monitoring

Key metrics ([DESIGN §4.2](../../docs/DESIGN.md#42-metrics-and-observability)):

- `oagw_proxy_cache_lookups_total{map, result}` — `result`: `hit`, `miss`, `joined`
- `oagw_proxy_cache_evictions_total{map, reason}` — `reason`: `change`, `ttl`, `capacity`
- `oagw_proxy_cache_changes_total{source}` — `local`, `remote`
- `oagw_proxy_cache_flushes_total{cause}` — `watch_reset`, `log_gap`, `log_reset`, `log_unreadable`, `watch_closed`
- `oagw_proxy_cache_loads_discarded_total{map}`
- `oagw_proxy_cache_publish_failures_total{reason}` — `append_failed`, `newer_log`, `publisher_stopped`
- `oagw_proxy_cache_entries{map}`

Alert on:

- Any `oagw_proxy_cache_publish_failures_total` increase
- Repeated `log_gap` flushes (an instance keeps falling behind the log)
- The healthcheck code `proxy_cache_watch_connecting` or `proxy_cache_watch_down` for longer than a few minutes (the cache's codes show before registry provisioning's when both are Degraded)
- The healthcheck code `proxy_cache_newer_log` after a rollback: delete the cluster key `oagw/proxy-config-changes` (ADR-0020 *Rolling upgrades*)
- The healthcheck code `cluster_readiness_slow` (the cluster readiness check has not answered for 10 s)

## Related ADRs

- [ADR: Proxy Configuration Cache](../../docs/ADR/0020-proxy-config-cache.md)
- [ADR: Optional Persistence](../../docs/ADR/0018-optional-persistence.md)
- [ADR: Control Plane Caching](../../docs/ADR/0007-data-plane-caching.md) (superseded by ADR-0020)
- [ADR: State Management](../../docs/ADR/0008-state-management.md) (its Data Plane L1 cache is replaced by ADR-0020)
- [ADR: Component Architecture](../../docs/ADR/0001-component-architecture.md)
