---
status: proposed
date: 2026-02-03
decision-makers: OAGW Team
---

# Concurrency Control — Semaphore-Based Limiting at Upstream, Route, and Tenant Levels


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Three Levels of Concurrency Limits](#three-levels-of-concurrency-limits)
  - [Implementation](#implementation)
  - [Upstream Concurrency Config](#upstream-concurrency-config)
  - [Route Concurrency Config](#route-concurrency-config)
  - [Tenant-Global Concurrency](#tenant-global-concurrency)
  - [Merge Strategy (Hierarchical Configuration)](#merge-strategy-hierarchical-configuration)
  - [Error Handling](#error-handling)
  - [Metrics](#metrics)
  - [Interaction with Other Systems](#interaction-with-other-systems)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Semaphore-based in-memory limiting](#semaphore-based-in-memory-limiting)
  - [Redis-based distributed limiting](#redis-based-distributed-limiting)
  - [No concurrency control](#no-concurrency-control)
- [More Information](#more-information)
  - [Connection Pool Sizing](#connection-pool-sizing)
  - [Database Schema Updates](#database-schema-updates)
  - [Configuration Validation](#configuration-validation)
  - [Defaults](#defaults)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-concurrency-control`

## Context and Problem Statement

OAGW needs concurrency control to limit the number of simultaneous in-flight requests, protecting upstream services from being overwhelmed, OAGW itself from resource exhaustion, and ensuring tenant isolation so one tenant cannot monopolize upstream capacity. Concurrency limiting differs from rate limiting: rate limiting controls requests per time window, concurrency limiting controls simultaneous active requests.

## Decision Drivers

* Prevent resource exhaustion from slow clients/upstreams
* Fair capacity sharing across tenants
* Low overhead tracking (<100ns per request)
* Graceful degradation when limits reached
* Observable metrics (current in-flight, limit utilization)
* Works with streaming requests (counted until completion)

## Considered Options

* Semaphore-based in-memory limiting (local-only)
* Redis-based distributed limiting
* No concurrency control (rely on rate limiting only)

## Decision Outcome

Chosen option: "Semaphore-based in-memory limiting with local-only coordination for MVP", because it provides low-overhead tracking with RAII permit pattern ensuring correct counter management.

### Three Levels of Concurrency Limits

1. **Upstream-level**: Total concurrent requests to an upstream (all routes combined)
2. **Route-level**: Concurrent requests to a specific route
3. **Tenant-level**: Concurrent requests from a tenant (across all upstreams)

All three limits are independent checks. A request must pass all applicable limits:
```text
Request → [Tenant Limit] → [Upstream Limit] → [Route Limit] → Execute
```

### Implementation

#### Semaphore-Based Limiting

Use in-memory semaphores for fast local checks:

```rust
struct ConcurrencyLimiter {
    max_concurrent: usize,
    in_flight: AtomicUsize,
}

impl ConcurrencyLimiter {
    fn try_acquire(&self) -> Result<Permit<'_>, ConcurrencyLimitExceeded> {
        let current = self.in_flight.fetch_add(1, Ordering::Relaxed);
        if current >= self.max_concurrent {
            self.in_flight.fetch_sub(1, Ordering::Relaxed);
            return Err(ConcurrencyLimitExceeded);
        }
        Ok(Permit { limiter: self })
    }
}

struct Permit<'a> {
    limiter: &'a ConcurrencyLimiter,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.limiter.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}
```

**Permit Pattern**: RAII guard ensures in-flight counter decrements even on error/panic.

#### Request Lifecycle

```text
1. Acquire tenant-global permit
2. Acquire upstream permit
3. Acquire route permit
4. Execute request
5. Permits auto-released on completion/error/timeout
```

**Streaming Requests**: Permit held until stream completes or client disconnects.

#### Distributed Coordination

**Local-Only Limiting** (Phase 1):

- Each OAGW node tracks in-flight independently
- Effective limit = `max_concurrent / node_count` (configured)
- Simple, low latency, no distributed state

**Distributed Limiting** (Phase 2):

- Use Redis or shared counter for accurate global limiting
- Increased latency (~1-5ms for distributed check)
- Required for strict enforcement

**Recommendation**: Start with local-only. Add distributed coordination only if needed.

### Upstream Concurrency Config

Add to `Upstream` type:

```json
{
  "concurrency_limit": {
    "sharing": "private",
    "max_concurrent": 100,
    "per_tenant_max": 20,
    "strategy": "reject"
  }
}
```

**Fields**:

- `sharing`: `"private"` | `"inherit"` | `"enforce"` (same semantics as rate_limit)
- `max_concurrent`: Total concurrent requests across all tenants (global limit)
- `per_tenant_max`: Max concurrent per individual tenant (fairness limit)
- `strategy`: `"reject"` | `"queue"` (queue behavior defined in ADR: Backpressure)

### Route Concurrency Config

Add to `Route` type:

```json
{
  "concurrency_limit": {
    "max_concurrent": 50
  }
}
```

**Fields**:

- `max_concurrent`: Max concurrent requests to this route

**Note**: Routes do not have `per_tenant_max` - use upstream-level for tenant isolation.

### Tenant-Global Concurrency

Configured at tenant level (not per-upstream):

```json
{
  "tenant_id": "uuid-123",
  "global_concurrency_limit": 200
}
```

**Scope**: Sum of all in-flight requests from this tenant across all upstreams.

### Merge Strategy (Hierarchical Configuration)

When descendant tenant binds to ancestor's upstream:

| Ancestor Sharing | Descendant Specifies | Effective Limit                        |
|------------------|----------------------|----------------------------------------|
| `private`        | —                    | Descendant must provide limit          |
| `inherit`        | No                   | Use ancestor's limit                   |
| `inherit`        | Yes                  | `min(ancestor, descendant)` (stricter) |
| `enforce`        | Any                  | `min(ancestor, descendant)` (stricter) |

**Rationale**: Always enforce the stricter limit (same as rate limiting) to prevent descendants from bypassing parent's capacity constraints.

### Error Handling

#### New Error Type

```json
{
  "type": "gts.x.core.errors.err.v1~x.oagw.concurrency_limit.exceeded.v1",
  "title": "Concurrency Limit Exceeded",
  "status": 503,
  "detail": "Upstream api.openai.com has reached max concurrent requests (100/100)",
  "instance": "/api/oagw/v1/proxy/api.openai.com/v1/chat",
  "upstream_id": "uuid-123",
  "host": "api.openai.com",
  "limit_type": "upstream",
  "current_in_flight": 100,
  "max_concurrent": 100,
  "retry_after_seconds": 1,
  "trace_id": "01J..."
}
```

**HTTP Headers**:

```http
HTTP/1.1 503 Service Unavailable
X-OAGW-Error-Source: gateway
Retry-After: 1
Content-Type: application/problem+json
```

**Retriable**: Yes (client should retry with backoff)

### Metrics

#### Core Metrics

```promql
# Current in-flight requests (gauge)
oagw_requests_in_flight{host, level} gauge
# level: "upstream", "route", "tenant"

# Concurrency limit rejections (counter)
oagw_concurrency_limit_exceeded_total{host, level} counter

# Concurrency utilization (0.0 to 1.0)
oagw_concurrency_usage_ratio{host, level} gauge
# = in_flight / max_concurrent

# Concurrency limit configuration (gauge)
oagw_concurrency_limit_max{host, level} gauge
```

#### Per-Tenant Tracking (Optional)

```promql
# Per-tenant in-flight (only if monitoring enabled)
oagw_tenant_requests_in_flight{tenant_id} gauge

# Note: High cardinality - enable only for monitoring/debugging
```

### Interaction with Other Systems

#### Rate Limiting

**Independent checks**: Both rate limit and concurrency limit must pass:

```text
Request → [Rate Limiter] → [Concurrency Limiter] → Execute
           └─ 429 if exceeded    └─ 503 if exceeded
```

**Order**: Check rate limit first (cheaper, rejects quota violations early).

#### Circuit Breaker

When circuit breaker is **OPEN**:

- Requests rejected immediately (no concurrency permit acquired)
- In-flight counter not affected

When circuit breaker is **HALF-OPEN**:

- Limited probe requests still count against concurrency limit
- Ensures probes don't overwhelm recovering upstream

#### Backpressure/Queueing

When `strategy: "queue"` is set:

- Failed `try_acquire()` adds request to queue (see ADR: Backpressure)
- When permit released, queue consumer acquires it

### Consequences

* Good, because prevents resource exhaustion
* Good, because improves stability under load
* Good, because fair capacity sharing via per-tenant limits
* Good, because clear observability (in-flight gauge, utilization ratio)
* Bad, because additional configuration complexity
* Bad, because potential false rejections during traffic spikes (mitigated by backpressure/queueing)
* Bad, because local-only limiting means limit is approximation across nodes

### Confirmation

**Unit Tests**:

- Semaphore acquire/release correctness
- RAII permit drop behavior
- Concurrent access from multiple threads

**Integration Tests**:

- Reject requests when limit reached
- Release permit on timeout/error/completion
- Streaming request lifecycle
- Hierarchical limit enforcement

**Load Tests**:

- Sustain max_concurrent requests without leaks
- Verify metrics accuracy
- Connection pool alignment

## Pros and Cons of the Options

### Semaphore-based in-memory limiting

* Good, because atomic operations only (<100ns per check)
* Good, because RAII pattern prevents counter leaks
* Good, because simple to implement and reason about
* Bad, because local-only is approximate across multiple nodes

### Redis-based distributed limiting

* Good, because accurate global enforcement
* Bad, because increased latency (~1-5ms per request)
* Bad, because Redis dependency

### No concurrency control

* Good, because no overhead
* Bad, because no protection against resource exhaustion
* Bad, because rate limiting alone doesn't prevent burst overload

## More Information

Implementation phases:
- **Phase 1**: Local limiting — in-memory semaphores, upstream/route-level limits, metrics
- **Phase 2**: Tenant isolation — per-tenant-max, tenant global limit, fairness
- **Phase 3**: Distributed coordination (optional) — Redis-based global counters

### Connection Pool Sizing

Concurrency limits should align with HTTP client connection pool size:

```json
{
  "upstream": {
    "concurrency_limit": {
      "max_concurrent": 100
    },
    "http_client": {
      "connection_pool": {
        "max_connections": 100,
        "max_idle_connections": 20,
        "idle_timeout": "90s"
      }
    }
  }
}
```

**Guidelines**:

- `max_connections` ≥ `max_concurrent` (to avoid blocking on pool exhaustion)
- `max_idle_connections` = 20-30% of `max_concurrent` (balance latency vs resources)
- Monitor `oagw_upstream_connections{state="waiting"}` for pool contention

### Database Schema Updates

#### Upstream Table

```sql
ALTER TABLE oagw_upstream
    ADD COLUMN concurrency_limit TEXT;

-- Example value:
-- {
--   "sharing": "enforce",
--   "max_concurrent": 100,
--   "per_tenant_max": 20,
--   "strategy": "reject"
-- }
```

#### Route Table

```sql
ALTER TABLE oagw_route
    ADD COLUMN concurrency_limit TEXT;

-- Example value:
-- {
--   "max_concurrent": 50
-- }
```

#### Tenant Table (Global Limit)

```sql
ALTER TABLE tenant
    ADD COLUMN oagw_global_concurrency_limit INTEGER;

-- Default: NULL (no limit)
```

### Configuration Validation

**Rules**:

1. `max_concurrent` must be > 0
2. `per_tenant_max` must be ≤ `max_concurrent`
3. Route limit must be ≤ upstream limit (if both specified)
4. Tenant global limit should be > sum of per-tenant-max across upstreams (warning, not error)

### Defaults

If not specified:

- **Upstream**: No concurrency limit (unlimited)
- **Route**: Inherits upstream limit
- **Tenant**: No global limit

**Recommendation**: Set conservative defaults at system level, allow overrides per upstream.

- [ADR: Rate Limiting](./0004-rate-limiting.md) — Time-based rate control
- [ADR: Backpressure and Queueing](./0012-backpressure-queueing.md) — Queue behavior when limits reached
- [ADR: Circuit Breaker](./0005-circuit-breaker.md) — Upstream health protection

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-fr-rate-limiting` — Concurrency limiting complements time-based rate limiting
* `cpt-cf-oagw-nfr-high-availability` — Prevents resource exhaustion and cascade failures
* `cpt-cf-oagw-nfr-multi-tenancy` — Per-tenant concurrency limits enforce fair sharing
