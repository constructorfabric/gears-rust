---
status: accepted
date: 2026-02-09
decision-makers: OAGW Team
---

# State Management — Data Plane L1 Cache and Rate Limiter Ownership


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [DP State](#dp-state)
  - [CP State](#cp-state)
  - [Request Flow with Caching](#request-flow-with-caching)
  - [Cache Invalidation](#cache-invalidation)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [DP Stateless](#dp-stateless)
  - [DP with L1 cache + rate limiters](#dp-with-l1-cache--rate-limiters)
  - [CP owns rate limiters](#cp-owns-rate-limiters)
- [Rationale](#rationale)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-state-management`

## Context and Problem Statement

With Data Plane (DP) calling Control Plane (CP) for config resolution, OAGW needs to decide how state is managed: whether DP should cache frequently-accessed configs, where rate limiters should live, and how to balance performance vs consistency.

## Decision Drivers

* Minimize latency on proxy hot path
* Reduce CP calls for frequently-accessed configs
* Simple rate limiting for MVP (no distributed coordination)
* Clear ownership of state between CP and DP

## Considered Options

* DP stateless (call CP for every request)
* DP with L1 cache + rate limiters owned by DP
* CP owns rate limiters (DP calls CP for rate checks)

## Decision Outcome

Chosen option: "DP with L1 cache + rate limiters owned by DP", because DP handles every proxy request and needs fast access to hot configs and rate limit state.

### DP State

- **L1 Cache**: Small in-memory LRU (1000 entries, no TTL, explicit invalidation). Caches upstream and route configs resolved from CP. Configurable via environment variable.
- **HTTP Client**: Shared client for calls to external (upstream) services.
- **Rate Limiters**: Per-instance token buckets owned by DP. DP has full request context (tenant, upstream, route).

```rust
pub struct DPState {
    // Small L1 cache for hot configs (1000 entries, LRU)
    hot_cache: Arc<Mutex<LruCache<CacheKey, ConfigValue>>>,

    // Connection to Control Plane
    cp_client: Arc<dyn ControlPlaneService>,

    // HTTP client for external services
    http_client: Arc<HttpClient>,

    // Rate limiters (per-upstream, per-route)
    rate_limiters: Arc<RateLimiterRegistry>,
}
```

### CP State

- **L1 Cache**: Larger in-memory LRU (10,000 entries). Authoritative cache backed by optional L2 (Redis) and database.
- **L2 Cache**: Optional Redis layer shared across instances.
- **Database Pool**: Connection pool for persistent storage.

```rust
pub struct CPState {
    // L1: In-memory cache (10k entries, LRU)
    l1_cache: Arc<Mutex<LruCache<CacheKey, ConfigValue>>>,

    // L2: Optional Redis (shared across instances)
    l2_cache: Option<Arc<RedisClient>>,

    // Database connection pool
    db_pool: Arc<DbConnectionPool>,
}
```

### Request Flow with Caching

```text
DP receives proxy request
├─ Check DP L1 cache for resolved (upstream, route) config
│  ├─ Hit: Use cached config (<1μs)
│  └─ Miss: Call CP.resolve_proxy_target(alias, method, path)
│           ├─ Single tenant hierarchy walk: alias shadowing + route match
│           ├─ Effective config merge (upstream < route < tenant)
│           ├─ CP checks L1/L2 cache, falls back to DB
│           └─ DP caches (EffectiveUpstream, MatchedRoute) in L1
├─ Execute auth plugin
├─ Check rate limiter (DP-owned)
├─ Execute guard/transform plugins
├─ HTTP call to external service
└─ Return response
```

### Cache Invalidation

On config write: CP writes to DB, flushes own caches, returns success. API Handler notifies DP to flush its L1 cache (or DP periodically syncs).

### Consequences

* Good, because fast path — DP serves hot configs from L1 (<1μs)
* Good, because reduced CP calls (only for cache misses)
* Good, because simple rate limiting (no distributed coordination for MVP)
* Bad, because DP L1 can temporarily diverge from CP (stale data)
* Bad, because per-instance rate limiting is not globally accurate

### Confirmation

Integration tests verify: DP L1 cache hit avoids CP call, config write triggers DP cache flush, rate limiter correctly counts per-instance requests.

## Pros and Cons of the Options

### DP Stateless

DP makes CP call for every request (no L1 cache).

* Good, because always consistent
* Bad, because too many CP calls, adds latency for hot configs

### DP with L1 cache + rate limiters

* Good, because fast reads (<1μs for cached configs)
* Good, because rate limiter has full request context
* Bad, because cache consistency lag after writes

### CP owns rate limiters

DP calls CP to check rate limits.

* Good, because centralized rate limit state
* Bad, because extra CP call per request, not worth the overhead for MVP

## Rationale

**Why DP has L1 cache**:

* DP handles every proxy request
* Reduces CP calls for hot configs
* <1μs access time for cached configs
* Small cache (1000 entries) has negligible memory overhead

**Why rate limiters in DP**:

* DP already has request context
* Avoids extra CP call per request
* Per-instance limiting is acceptable for MVP

**Why CP is authoritative cache**:

* CP owns database access
* CP can optimize cache invalidation during writes
* DP L1 is just optimization layer

## More Information

**Risk**: DP L1 cache becomes stale after config write.
**Mitigation**: Explicit cache invalidation from CP on config writes (no TTL; entries persist until invalidated).

**Risk**: Per-instance rate limiting less accurate than distributed.
**Mitigation**: Acceptable for MVP. Future: Add Redis-backed distributed rate limiter as DP extension.

- [ADR: Component Architecture](./0001-component-architecture.md)
- [ADR: Control Plane Caching](./0007-data-plane-caching.md)
- [ADR: Request Routing](./0002-request-routing.md)

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-nfr-low-latency` — DP L1 cache provides <1μs config lookups on hot path
* `cpt-cf-oagw-fr-rate-limiting` — Rate limiters owned by DP for per-instance enforcement
* `cpt-cf-oagw-fr-request-proxy` — Caching strategy optimizes proxy request execution
