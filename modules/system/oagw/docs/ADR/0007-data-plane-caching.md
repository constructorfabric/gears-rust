---
status: accepted
date: 2026-02-09
decision-makers: OAGW Team
---

# Control Plane Caching — Multi-Layer L1/L2 Strategy


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Cache Layers](#cache-layers)
  - [Lookup Flow](#lookup-flow)
  - [Cache Keys](#cache-keys)
  - [Cache Invalidation](#cache-invalidation)
  - [Deployment Modes](#deployment-modes)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [L1 Only (In-Memory)](#l1-only-in-memory)
  - [L2 Only (Redis)](#l2-only-redis)
  - [Multi-layer L1 + L2 + Database](#multi-layer-l1--l2--database)
  - [Write-Through Cache](#write-through-cache)
  - [Risks](#risks)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-data-plane-caching`

## Context and Problem Statement

Control Plane handles config resolution for Data Plane during proxy requests. Configuration data (upstreams, routes, plugins) is read-heavy and changes infrequently. A caching strategy is needed that minimizes database load, provides fast lookups (<1ms for hot configs), supports both single-exec and microservice modes, and handles cache invalidation on config writes.

## Decision Drivers

* Fast lookups for hot configs (<1μs L1, ~1-2ms L2)
* Reduced database load (queries only on cache miss)
* Support for both single-exec (no Redis) and microservice (shared L2) deployment modes
* Correct cache invalidation on config writes

## Considered Options

* L1 only (in-memory)
* L2 only (Redis)
* Multi-layer L1 + optional L2 + Database
* Write-through cache

## Decision Outcome

Chosen option: "Multi-layer caching: L1 (in-memory) + optional L2 (Redis) + Database", because it provides the fastest reads for hot configs while supporting both deployment modes.

### Cache Layers

| Layer | Scope | Capacity | TTL | Access Time | Notes |
|---|---|---|---|---|---|
| L1 (In-Memory) | Per-instance LRU | 10,000 entries | No TTL (LRU eviction) | <1μs | |
| L2 (Redis, optional) | Shared across instances | Unbounded | 5 minutes | ~1-2ms | MessagePack serialization |
| Database (PostgreSQL) | Source of truth (JSON text) | Unlimited | N/A | ~5-10ms | Queried only on L1+L2 miss |

### Lookup Flow

```rust
async fn get_config(key: &CacheKey) -> Result<ConfigValue> {
    // Check L1
    if let Some(value) = l1_cache.get(key) {
        return Ok(value);  // <1μs
    }

    // Check L2 (if enabled)
    if let Some(redis) = l2_cache {
        if let Some(value) = redis.get(key).await? {
            l1_cache.insert(key, value.clone());
            return Ok(value);  // ~1-2ms
        }
    }

    // Query DB
    let value = db.query(key).await?;  // ~5-10ms

    // Populate caches
    l1_cache.insert(key, value.clone());
    if let Some(redis) = l2_cache {
        redis.set(key, &value, TTL_5MIN).await?;
    }

    Ok(value)
}
```

Caches are lazily populated on read (no proactive warming).

### Cache Keys

- `upstream:{tenant_id}:{alias}` → UpstreamConfig
- `route:{upstream_id}:{method}:{path_prefix}` → RouteConfig
- `plugin:{plugin_id}` → Plugin definition

### Cache Invalidation

On config write (e.g., `PUT /upstreams/{id}`): (1) CP writes to database, (2) CP flushes L1 for affected keys, (3) CP flushes L2 (if enabled), (4) CP returns success, (5) DP flushes its own L1 cache (notified by CP or periodic sync).

### Deployment Modes

- **Single-Exec**: L1 only (no Redis needed)
- **Microservice**: L1 + L2 (Redis shared across instances)

### Consequences

* Good, because fast lookups for hot configs (<1μs L1)
* Good, because reduced database load
* Good, because shared cache in microservice mode (L2)
* Good, because simple deployment in single-exec mode (no Redis)
* Bad, because cache invalidation complexity (must flush L1 and L2)
* Bad, because Redis dependency in microservice mode
* Bad, because potential stale data during cache TTL window

### Confirmation

Integration tests verify: L1 cache hit returns correct config, L1 miss falls through to L2/DB, config write flushes both L1 and L2, single-exec mode works without Redis.

## Pros and Cons of the Options

### L1 Only (In-Memory)

* Good, because simplest implementation, fastest reads
* Bad, because in microservice mode, each instance hits DB independently (high load)

### L2 Only (Redis)

* Good, because shared across instances
* Bad, because slower than L1 (serialization overhead), unnecessary for single-exec

### Multi-layer L1 + L2 + Database

* Good, because optimal read performance (L1 for speed, L2 for sharing)
* Good, because optional L2 keeps single-exec simple
* Bad, because invalidation must cover multiple layers

### Write-Through Cache

* Good, because cache always consistent
* Bad, because complicates writes, doesn't help read-heavy workload significantly

### Risks

**Risk**: Redis unavailability causes L2 miss, increased DB load.
**Mitigation**: L1 cache still active (10k entries), DB connection pool limits concurrent queries.

## More Information

- [ADR: Component Architecture](./0001-component-architecture.md)
- [ADR: State Management](./0008-state-management.md)

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-nfr-low-latency` — L1 cache provides <1μs config lookups on hot path
* `cpt-cf-oagw-fr-request-proxy` — Config resolution during proxy request execution
