---
status: accepted
date: 2026-02-03
decision-makers: OAGW Team
---

# Rate Limiting — Token Bucket with Dual-Rate Configuration and Hierarchical Budget Allocation


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
  - [Algorithm Comparison](#algorithm-comparison)
  - [Configuration Options](#configuration-options)
  - [Hierarchy Options](#hierarchy-options)
  - [Distribution Options](#distribution-options)
- [Decision Outcome](#decision-outcome)
  - [1. Algorithm: Token Bucket (default), Sliding Window (optional)](#1-algorithm-token-bucket-default-sliding-window-optional)
  - [2. Configuration: Dual-Rate](#2-configuration-dual-rate)
  - [3. Inheritance: Hierarchical Budget Allocation](#3-inheritance-hierarchical-budget-allocation)
  - [4. Distribution: Hybrid Local + Periodic Sync](#4-distribution-hybrid-local--periodic-sync)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Token Bucket algorithm](#token-bucket-algorithm)
  - [Sliding Window algorithm](#sliding-window-algorithm)
  - [Flat Configuration (single rate/window)](#flat-configuration-single-ratewindow)
  - [Dual-Rate Configuration](#dual-rate-configuration)
  - [Independent Limits (no sharing)](#independent-limits-no-sharing)
  - [Hierarchical Budget Allocation](#hierarchical-budget-allocation)
  - [Local-Only distribution](#local-only-distribution)
  - [Centralized Redis](#centralized-redis)
  - [Hybrid Local + Periodic Sync](#hybrid-local--periodic-sync)
- [Schema Changes](#schema-changes)
  - [Upstream Rate Limit (updated)](#upstream-rate-limit-updated)
- [Examples](#examples)
  - [Example 1: System → Partner → Tenant Hierarchy](#example-1-system--partner--tenant-hierarchy)
  - [Example 2: Shared Pool Among Tenants](#example-2-shared-pool-among-tenants)
  - [Example 3: Cost-Based Rate Limiting](#example-3-cost-based-rate-limiting)
- [Implementation Notes](#implementation-notes)
  - [Token Bucket Algorithm](#token-bucket-algorithm-1)
  - [Redis Sync Protocol](#redis-sync-protocol)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-rate-limiting`

## Context and Problem Statement

OAGW needs a rate limiting strategy that addresses three concerns: (1) algorithm selection for controlling request rates, (2) hierarchical inheritance of limits through tenant hierarchy (system → partner → tenant), and (3) distributed state synchronization of rate limit counters across OAGW nodes.

## Decision Drivers

* Low latency impact (<1ms for rate check)
* Accurate enforcement across distributed nodes
* Hierarchical budget allocation (parent can cap children)
* Fair sharing among tenants
* Burst handling without starving steady traffic
* Observability (remaining quota, reset time)

## Considered Options

* Algorithm: Token Bucket (default) vs Leaky Bucket vs Fixed Window vs Sliding Window
* Configuration: Flat config vs Token Bucket config vs Dual-Rate config
* Inheritance: Independent limits vs Hierarchical Budget Allocation vs Shared Pool with Guarantees
* Distribution: Local-Only vs Centralized Redis vs Hybrid Local + Periodic Sync vs Quota-Based

### Algorithm Comparison

| Algorithm          | Pros                                    | Cons                                 | Use Case                |
|--------------------|-----------------------------------------|--------------------------------------|-------------------------|
| **Token Bucket**   | Allows bursts, simple, memory efficient | Burst at window boundary             | Default for most APIs   |
| **Leaky Bucket**   | Smooth output rate                      | No burst tolerance, queue management | Steady-rate backends    |
| **Fixed Window**   | Simple, predictable reset               | 2x burst at boundary                 | Simple quotas           |
| **Sliding Window** | No boundary burst, accurate             | Slightly more compute                | Strict rate enforcement |

### Configuration Options

**Option A: Flat Configuration**

```json
{
  "rate_limit": {
    "rate": 1000,
    "window": "minute",
    "capacity": 100,
    "scope": "tenant"
  }
}
```

- Simple but limited; no algorithm choice; no burst/sustained distinction

**Option B: Token Bucket Configuration**

```json
{
  "rate_limit": {
    "algorithm": "token_bucket",
    "tokens_per_second": 100,
    "bucket_capacity": 500,
    "scope": "tenant"
  }
}
```

- Explicit algorithm; clear burst (capacity) vs sustained (tokens_per_second) distinction; matches AWS/Kong model

**Option C: Dual-Rate Configuration (Recommended)**

```json
{
  "rate_limit": {
    "algorithm": "token_bucket",
    "sustained": { "rate": 100, "window": "second" },
    "burst": { "capacity": 500 },
    "scope": "tenant",
    "strategy": "reject"
  }
}
```

- Separates sustained rate from burst capacity; human-readable window units; extensible for future algorithms

### Hierarchy Options

**Option A: Independent Limits (No Sharing)**

Each level sets own limit. No coordination.

```text
System: 10,000/min (global cap)
Partner A: 5,000/min (own limit)
  Tenant A1: 1,000/min
  Tenant A2: 1,000/min
```

**Option B: Hierarchical Budget Allocation (Recommended)**

Parent allocates budget to children. Children cannot exceed allocation.

```text
System: 10,000/min (total capacity)
├── Partner A: 5,000/min (allocated by system)
│   ├── Tenant A1: 2,000/min (allocated by partner)
│   └── Tenant A2: 1,000/min (allocated by partner)
│   └── (unallocated: 2,000/min - reserved or shared pool)
└── Partner B: 3,000/min (allocated by system)
└── (unallocated: 2,000/min - system reserve)
```

**Option C: Shared Pool with Guarantees**

Parent defines guaranteed minimum + shared pool.

```json
{
  "rate_limit": {
    "budget": {
      "total": 5000,
      "guaranteed": {
        "tenant_a1": 1000,
        "tenant_a2": 500
      },
      "shared_pool": 3500
    }
  }
}
```

- Complex but fair; guarantees minimum for each tenant; shared pool for burst/overflow

### Distribution Options

**Option A: Local-Only (No Sync)**

Each node tracks own counters. Effective limit = `configured_limit / node_count`.

**Option B: Centralized Store (Redis)**

```text
Node 1 ──┐
Node 2 ──┼── Redis (atomic INCR + EXPIRE)
Node 3 ──┘
```

**Option C: Hybrid Local + Periodic Sync (Recommended)**

Local token bucket with periodic sync to central store.

```text
┌─────────────────────────────────────────────────────────┐
│  OAGW Node                                              │
│  ┌─────────────────┐    ┌─────────────────────────────┐ │
│  │ Local Bucket    │◄───│ Sync Thread (every 100ms)   │ │
│  │ - Fast check    │    │ - Push local count          │ │
│  │ - Approximate   │    │ - Pull global count         │ │
│  └─────────────────┘    │ - Adjust local tokens       │ │
│                         └─────────────────────────────┘ │
└─────────────────────────────────────────────────────────┘
                              │
                              ▼
                    ┌─────────────────┐
                    │  Redis/Valkey   │
                    │  (global state) │
                    └─────────────────┘
```

**Sync algorithm**:

1. Each node maintains local token bucket
2. Periodically (configurable, default 100ms):
    - Push: `INCRBY global_counter local_consumed`
    - Pull: `GET global_counter`
    - Adjust local bucket based on global state
3. Local check is fast (in-memory)
4. Global accuracy within sync interval

**Trade-offs**:

- Sync interval short → more accurate, more Redis load
- Sync interval long → less accurate, less Redis load
- Burst can exceed limit by `burst_capacity * node_count` in worst case

**Option D: Quota-Based (Envoy-style)**

Central service allocates quota to nodes. Nodes request more when depleted.

```text
Node 1: "Give me quota for tenant X"
Quota Service: "Here's 100 tokens, valid for 10s"
Node 1: Uses tokens locally, requests more when low
```

- Most accurate; complex implementation; requires quota service

## Decision Outcome

Chosen option: Token Bucket algorithm with Dual-Rate configuration, Hierarchical Budget Allocation, and Hybrid Local + Periodic Sync for distributed state.

### 1. Algorithm: Token Bucket (default), Sliding Window (optional)

Token bucket is industry standard (AWS API Gateway, Kong, Envoy), handles bursts well, and is simple to implement.

### 2. Configuration: Dual-Rate

Separates sustained rate from burst capacity with human-readable window units:

```json
{
  "rate_limit": {
    "sharing": "enforce",
    "algorithm": "token_bucket",
    "sustained": {
      "rate": 100,
      "window": "second"
    },
    "burst": {
      "capacity": 500
    },
    "scope": "tenant",
    "strategy": "reject",
    "response_headers": true
  }
}
```

**Fields**:

| Field              | Type | Default          | Description                                              |
|--------------------|------|------------------|----------------------------------------------------------|
| `sharing`          | enum | `private`        | `private`, `inherit`, `enforce`                          |
| `algorithm`        | enum | `token_bucket`   | `token_bucket`, `sliding_window`                         |
| `sustained.rate`   | int  | required         | Tokens replenished per window                            |
| `sustained.window` | enum | `second`         | `second`, `minute`, `hour`, `day`                        |
| `burst.capacity`   | int  | `sustained.rate` | Max bucket size (burst allowance)                        |
| `scope`            | enum | `tenant`         | Counter scope: `global`, `tenant`, `user`, `ip`, `route` |
| `strategy`         | enum | `reject`         | `reject` (429), `queue`, `degrade`                       |
| `response_headers` | bool | `true`           | Include `X-RateLimit-*` headers                          |
| `cost`             | int  | `1`              | Tokens consumed per request                              |

### 3. Inheritance: Hierarchical Budget Allocation

Parent allocates budget to children. Effective limit = `min(own_limit, parent_effective_limit)`. Sum of child allocations ≤ parent allocation (configurable overcommit ratio).

**Schema extension**:

```json
{
  "rate_limit": {
    "sharing": "enforce",
    "budget": {
      "mode": "allocated",
      "total": 5000,
      "overcommit_ratio": 1.0
    },
    "sustained": { "rate": 100, "window": "second" },
    "burst": { "capacity": 500 }
  }
}
```

**Budget modes**:

| Mode        | Description                                              |
|-------------|----------------------------------------------------------|
| `unlimited` | No budget tracking (default for leaf tenants)            |
| `allocated` | Parent allocates fixed budget to children                |
| `shared`    | Children share parent's budget (first-come-first-served) |

**Inheritance rules**:

| Parent Sharing | Child Specifies | Effective Limit                             |
|----------------|-----------------|---------------------------------------------|
| `private`      | any             | Child's limit only                          |
| `inherit`      | none            | Parent's limit                              |
| `inherit`      | own limit       | `min(parent, child)`                        |
| `enforce`      | any             | `min(parent, child)` - cannot exceed parent |

**Budget validation on child creation**:

```text
Parent budget: 5000/min
Existing children: A (2000), B (1000)
New child C requests: 3000

If overcommit_ratio = 1.0:
  Sum = 2000 + 1000 + 3000 = 6000 > 5000 → REJECT

If overcommit_ratio = 1.5:
  Sum = 6000 ≤ 5000 * 1.5 = 7500 → ALLOW (with warning)
```

### 4. Distribution: Hybrid Local + Periodic Sync

MVP: Per-instance rate limiting in Data Plane (no distributed coordination). Future: Redis-backed distributed rate limiting with configurable sync interval (default 100ms).

Rate limiting executes in **Data Plane (DP)**. Configuration is resolved from Control Plane (CP) caches during upstream/route resolution.

**Configuration**:

```json
{
  "rate_limit_sync": {
    "enabled": true,
    "backend": "redis",
    "sync_interval_ms": 100,
    "fallback_on_error": "local_only"
  }
}
```

**Behavior**:

- Normal: Local bucket + periodic sync
- Redis unavailable: Fall back to local-only (degraded accuracy)
- High load: Adaptive sync interval (more frequent when near limit)

**Redis key structure**:

```text
oagw:ratelimit:{scope}:{identifier}:{window} = {count}
oagw:ratelimit:tenant:uuid-123:minute:202601301530 = 4523
```

> **Note:** Minute-bucket keys must use **YYYYMMDDHHMM** (12 digits) to avoid
> confusion with hour-level granularity and prevent incorrect aggregation.

### Consequences

* Good, because clear separation of sustained rate vs burst capacity
* Good, because hierarchical budget prevents child from exceeding parent allocation
* Good, because hybrid sync balances accuracy vs performance
* Good, because standard response headers (RFC 6585 / draft-ietf-httpapi-ratelimit-headers) for client integration
* Bad, because Redis dependency for distributed accuracy
* Bad, because complexity in budget allocation validation
* Bad, because sync interval introduces accuracy trade-off
* Risk, Redis failure degrades to local-only (less accurate)
* Risk, overcommit can lead to contention under high load
* Risk, clock skew between nodes affects sliding window accuracy

### Confirmation

Integration tests verify: (1) token bucket allows bursts up to capacity, (2) hierarchical limits enforce `min(parent, child)`, (3) 429 responses include `X-RateLimit-*` and `Retry-After` headers.

## Pros and Cons of the Options

### Token Bucket algorithm

* Good, because allows bursts up to bucket capacity
* Good, because simple, memory efficient
* Good, because industry standard
* Bad, because burst possible at window boundary

### Sliding Window algorithm

* Good, because no boundary burst, accurate
* Bad, because slightly more compute per check

### Flat Configuration (single rate/window)

* Good, because simple
* Bad, because no burst/sustained distinction
* Bad, because no algorithm choice

### Dual-Rate Configuration

* Good, because separates sustained rate from burst capacity
* Good, because human-readable window units
* Good, because extensible for future algorithms

### Independent Limits (no sharing)

* Good, because simple implementation
* Bad, because partner's tenants can exceed partner's allocation without enforcement

### Hierarchical Budget Allocation

* Good, because enforces parent capacity constraints on children
* Good, because supports overcommit ratio for flexibility
* Bad, because more complex validation on child creation

### Local-Only distribution

* Good, because no external dependency, low latency
* Bad, because inaccurate if traffic not evenly distributed

### Centralized Redis

* Good, because accurate global enforcement
* Bad, because added latency (~1-2ms per request), Redis becomes SPOF

### Hybrid Local + Periodic Sync

* Good, because local check is fast (in-memory), global accuracy within sync interval
* Bad, because burst can exceed limit by `burst_capacity * node_count` in worst case

## Schema Changes

### Upstream Rate Limit (updated)

```json
{
  "rate_limit": {
    "type": "object",
    "properties": {
      "sharing": {
        "type": "string",
        "enum": [ "private", "inherit", "enforce" ],
        "default": "private"
      },
      "algorithm": {
        "type": "string",
        "enum": [ "token_bucket", "sliding_window" ],
        "default": "token_bucket"
      },
      "sustained": {
        "type": "object",
        "properties": {
          "rate": { "type": "integer", "minimum": 1 },
          "window": {
            "type": "string",
            "enum": [ "second", "minute", "hour", "day" ],
            "default": "second"
          }
        },
        "required": [ "rate" ]
      },
      "burst": {
        "type": "object",
        "properties": {
          "capacity": { "type": "integer", "minimum": 1 }
        }
      },
      "budget": {
        "type": "object",
        "properties": {
          "mode": {
            "type": "string",
            "enum": [ "unlimited", "allocated", "shared" ],
            "default": "unlimited"
          },
          "total": { "type": "integer", "minimum": 1 },
          "overcommit_ratio": {
            "type": "number",
            "minimum": 1.0,
            "maximum": 2.0,
            "default": 1.0
          }
        }
      },
      "scope": {
        "type": "string",
        "enum": [ "global", "tenant", "user", "ip", "route" ],
        "default": "tenant"
      },
      "strategy": {
        "type": "string",
        "enum": [ "reject", "queue", "degrade" ],
        "default": "reject"
      },
      "cost": {
        "type": "integer",
        "minimum": 1,
        "default": 1
      },
      "response_headers": {
        "type": "boolean",
        "default": true
      }
    },
    "required": [ "sustained" ]
  }
}
```

## Examples

### Example 1: System → Partner → Tenant Hierarchy

**System level** (global cap):

```json
{
  "rate_limit": {
    "sharing": "enforce",
    "sustained": { "rate": 10000, "window": "minute" },
    "burst": { "capacity": 1000 },
    "budget": { "mode": "allocated", "total": 10000 }
  }
}
```

**Partner level** (allocated 5000/min by system):

```json
{
  "rate_limit": {
    "sharing": "enforce",
    "sustained": { "rate": 5000, "window": "minute" },
    "burst": { "capacity": 500 },
    "budget": { "mode": "allocated", "total": 5000, "overcommit_ratio": 1.2 }
  }
}
```

**Tenant level** (allocated 1000/min by partner):

```json
{
  "rate_limit": {
    "sustained": { "rate": 1000, "window": "minute" },
    "burst": { "capacity": 100 }
  }
}
```

**Effective for tenant**:

- Sustained: `min(system:10000, partner:5000, tenant:1000)` = 1000/min
- Burst: `min(system:1000, partner:500, tenant:100)` = 100

### Example 2: Shared Pool Among Tenants

**Partner with shared pool**:

```json
{
  "rate_limit": {
    "sharing": "inherit",
    "sustained": { "rate": 5000, "window": "minute" },
    "budget": { "mode": "shared", "total": 5000 }
  }
}
```

Tenants A, B, C share 5000/min pool. First-come-first-served. No individual guarantees.

### Example 3: Cost-Based Rate Limiting

Different endpoints have different costs:

```json
{
  "routes": [
    {
      "path": "/v1/chat/completions",
      "rate_limit": { "cost": 10 }
    },
    {
      "path": "/v1/models",
      "rate_limit": { "cost": 1 }
    }
  ]
}
```

Tenant with 1000 tokens/min can call:

- 100 chat completions, or
- 1000 model listings, or
- 50 chat + 500 models

## Implementation Notes

### Token Bucket Algorithm

```rust
struct TokenBucket {
    tokens: f64,
    last_update: Instant,
    capacity: f64,
    refill_rate: f64, // tokens per second
}

impl TokenBucket {
    fn try_acquire(&mut self, cost: u32) -> bool {
        self.refill();
        if self.tokens >= cost as f64 {
            self.tokens -= cost as f64;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_update = now;
    }
}
```

### Redis Sync Protocol

```rust
// Every sync_interval_ms
async fn sync_with_redis(local: &mut TokenBucket, key: &str, redis: &Redis) {
    // 1. Push local consumption
    let consumed = local.consumed_since_last_sync();
    redis.incrby(key, consumed).await;

    // 2. Pull global state
    let global_count = redis.get(key).await;

    // 3. Adjust local bucket
    // If global is higher than expected, reduce local tokens
    local.adjust_from_global(global_count);
}
```

## More Information

Response headers follow RFC 6585 / draft-ietf-httpapi-ratelimit-headers:
```http
X-RateLimit-Limit: 100
X-RateLimit-Remaining: 0
X-RateLimit-Reset: 1706626800
Retry-After: 30
```

- [OAGW Design Document](../DESIGN.md)
- [ADR: Resource Identification](./0010-resource-identification.md)
- [ADR: Concurrency Control](./0011-concurrency-control.md)
- [ADR: Backpressure and Queueing](./0012-backpressure-queueing.md)
- [Kong Rate Limiting](https://konghq.com/blog/engineering/how-to-design-a-scalable-rate-limiting-algorithm)
- [Envoy Global Rate Limiting](https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/other_features/global_rate_limiting)
- [AWS API Gateway Throttling](https://docs.aws.amazon.com/apigateway/latest/developerguide/api-gateway-request-throttling.html)
- [IETF RateLimit Headers Draft](https://datatracker.ietf.org/doc/draft-ietf-httpapi-ratelimit-headers/)

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-fr-rate-limiting` — Rate limiting algorithm, configuration, and enforcement
* `cpt-cf-oagw-fr-hierarchical-config` — Hierarchical budget allocation and sharing modes
* `cpt-cf-oagw-nfr-low-latency` — <1ms rate check latency via local token bucket
* `cpt-cf-oagw-usecase-rate-limit-exceeded` — 429 response with Retry-After header
