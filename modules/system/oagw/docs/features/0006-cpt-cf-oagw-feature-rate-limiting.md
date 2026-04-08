# Feature: Rate Limiting & Resilience


<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [Rate-Limited Proxy Request](#rate-limited-proxy-request)
  - [Configure Rate Limits](#configure-rate-limits)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Token Bucket Rate Check](#token-bucket-rate-check)
  - [Hierarchical Rate Limit Merge](#hierarchical-rate-limit-merge)
  - [Circuit Breaker State Evaluation](#circuit-breaker-state-evaluation)
  - [Concurrency Permit Acquisition](#concurrency-permit-acquisition)
  - [Backpressure Queue Management](#backpressure-queue-management)
- [4. States (CDSL)](#4-states-cdsl)
  - [Circuit Breaker State Machine](#circuit-breaker-state-machine)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Implement Token Bucket Rate Limiter](#implement-token-bucket-rate-limiter)
  - [Implement Rate Limit Response Headers](#implement-rate-limit-response-headers)
  - [Implement Hierarchical Rate Limit Merge](#implement-hierarchical-rate-limit-merge)
  - [Implement Circuit Breaker State Machine](#implement-circuit-breaker-state-machine)
  - [Implement Concurrency Control](#implement-concurrency-control)
  - [Implement Backpressure Queueing](#implement-backpressure-queueing)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Additional Context](#7-additional-context)
  - [Performance Considerations](#performance-considerations)
  - [Distributed State](#distributed-state)
  - [Deliberate Omissions](#deliberate-omissions)

<!-- /toc -->

- [ ] `p2` - **ID**: `cpt-cf-oagw-featstatus-rate-limiting-implemented`

<!-- reference to DECOMPOSITION entry -->
- [ ] `p2` - `cpt-cf-oagw-feature-rate-limiting`

## 1. Feature Context

### 1.1 Overview

Implement rate limiting at upstream and route levels with token bucket algorithm, configurable strategies, circuit breaker state machine, concurrency control, and backpressure queueing.

### 1.2 Purpose

Prevents abuse, cost overruns, and protects external service agreements. Maintains high availability through circuit breaker pattern. Covers `cpt-cf-oagw-fr-rate-limiting`, `cpt-cf-oagw-nfr-high-availability`.

### 1.3 Actors

| Actor | Role in Feature |
|-------|-----------------|
| `cpt-cf-oagw-actor-platform-operator` | Configures rate limits and circuit breaker thresholds |
| `cpt-cf-oagw-actor-tenant-admin` | Sets tenant-scoped rate limits (subject to ancestor enforcement) |
| `cpt-cf-oagw-actor-app-developer` | Receives 429/503 responses when limits are exceeded |

### 1.4 References

- **PRD**: [PRD.md](../PRD.md)
- **Design**: [DESIGN.md](../DESIGN.md)
- **ADR**: [ADR/0004-rate-limiting.md](../ADR/0004-rate-limiting.md), [ADR/0005-circuit-breaker.md](../ADR/0005-circuit-breaker.md), [ADR/0011-concurrency-control.md](../ADR/0011-concurrency-control.md), [ADR/0012-backpressure-queueing.md](../ADR/0012-backpressure-queueing.md)
- **Dependencies**: `cpt-cf-oagw-feature-proxy-engine`

## 2. Actor Flows (CDSL)

### Rate-Limited Proxy Request

- [ ] `p2` - **ID**: `cpt-cf-oagw-flow-rate-limited-proxy`

**Actor**: `cpt-cf-oagw-actor-app-developer`

**Success Scenarios**:
- Request passes rate limit check and is forwarded to upstream
- Rate limit response headers (`X-RateLimit-Limit`, `X-RateLimit-Remaining`, `X-RateLimit-Reset`) are included in response
- Request passes concurrency limit check (permit acquired, auto-released on completion)

**Error Scenarios**:
- Rate limit exceeded with reject strategy (429 RateLimitExceeded + `Retry-After`)
- Rate limit exceeded with queue strategy — request queued, then times out (503 QueueTimeout)
- Concurrency limit exceeded at upstream level (503 ConcurrencyLimitExceeded)
- Concurrency limit exceeded at route level (503 ConcurrencyLimitExceeded)
- Concurrency limit exceeded at tenant-global level (503 ConcurrencyLimitExceeded)
- Circuit breaker open for upstream (503 CircuitBreakerOpen + `Retry-After`)
- Circuit breaker half-open probe fails — circuit re-opens

**Steps**:
1. [ ] - `p2` - Actor sends proxy request via `cpt-cf-oagw-flow-proxy-request` (alias resolution and route matching complete) - `inst-rl-1`
2. [ ] - `p2` - Resolve effective rate limit config via `cpt-cf-oagw-algo-hierarchical-rate-limit-merge` for upstream and matched route - `inst-rl-2`
3. [ ] - `p2` - Check circuit breaker state via `cpt-cf-oagw-algo-circuit-breaker-evaluation` - `inst-rl-3`
4. [ ] - `p2` - **IF** circuit breaker is OPEN - `inst-rl-4`
   1. [ ] - `p2` - **RETURN** 503 CircuitBreakerOpen with `Retry-After` and `X-Circuit-State: OPEN` via `cpt-cf-oagw-algo-error-source-classification` - `inst-rl-4a`
5. [ ] - `p2` - Check rate limit via `cpt-cf-oagw-algo-token-bucket-check` with effective rate config and request cost - `inst-rl-5`
6. [ ] - `p2` - **IF** rate limit exceeded - `inst-rl-6`
   1. [ ] - `p2` - **IF** strategy = `reject` - `inst-rl-6a`
      1. [ ] - `p2` - **RETURN** 429 RateLimitExceeded with `X-RateLimit-*` headers and `Retry-After` - `inst-rl-6a1`
   2. [ ] - `p2` - **IF** strategy = `queue` - `inst-rl-6b`
      1. [ ] - `p2` - Enqueue request via `cpt-cf-oagw-algo-backpressure-queue` - `inst-rl-6b1`
      2. [ ] - `p2` - **IF** queue full or timeout expires - `inst-rl-6b2`
         1. [ ] - `p2` - **RETURN** 503 QueueTimeout or QueueFull with `Retry-After` - `inst-rl-6b2a`
7. [ ] - `p2` - Acquire concurrency permits via `cpt-cf-oagw-algo-concurrency-permit-acquisition` (tenant → upstream → route) - `inst-rl-7`
8. [ ] - `p2` - **IF** any concurrency limit exceeded - `inst-rl-8`
   1. [ ] - `p2` - **IF** strategy = `reject` - `inst-rl-8a`
      1. [ ] - `p2` - **RETURN** 503 ConcurrencyLimitExceeded with `Retry-After` - `inst-rl-8a1`
   2. [ ] - `p2` - **IF** strategy = `queue` - `inst-rl-8b`
      1. [ ] - `p2` - Enqueue request via `cpt-cf-oagw-algo-backpressure-queue` - `inst-rl-8b1`
9. [ ] - `p2` - Continue proxy flow (auth → guards → transform → upstream call) per `cpt-cf-oagw-flow-proxy-request` - `inst-rl-9`
10. [ ] - `p2` - On upstream response: evaluate circuit breaker via `cpt-cf-oagw-algo-circuit-breaker-evaluation` (record success/failure) - `inst-rl-10`
11. [ ] - `p2` - Concurrency permits auto-released via RAII Drop - `inst-rl-11`
12. [ ] - `p2` - Include `X-RateLimit-*` response headers if `response_headers: true` in rate limit config - `inst-rl-12`
13. [ ] - `p2` - **RETURN** upstream response to caller - `inst-rl-13`

### Configure Rate Limits

- [ ] `p2` - **ID**: `cpt-cf-oagw-flow-configure-rate-limits`

**Actor**: `cpt-cf-oagw-actor-platform-operator`, `cpt-cf-oagw-actor-tenant-admin`

**Success Scenarios**:
- Rate limit config is set on upstream via management API (PUT /api/oagw/v1/upstreams/{id})
- Rate limit config is set on route via management API (PUT /api/oagw/v1/routes/{id})
- Circuit breaker config is set on upstream
- Concurrency limit config is set on upstream or route
- Budget allocation validated against parent's allocation

**Error Scenarios**:
- Budget allocation exceeds parent's available capacity (400 ValidationError)
- Child rate limit exceeds enforced ancestor limit (400 ValidationError)
- Invalid configuration values (400 ValidationError)

**Steps**:
1. [ ] - `p2` - Actor sends PUT /api/oagw/v1/upstreams/{id} or PUT /api/oagw/v1/routes/{id} with `rate_limit`, `circuit_breaker`, or `concurrency_limit` in request body - `inst-cfg-1`
2. [ ] - `p2` - Validate rate limit config: `sustained.rate` > 0, `burst.capacity` ≥ 1, `scope` is valid enum, `strategy` is valid enum - `inst-cfg-2`
3. [ ] - `p2` - **IF** `budget.mode` = `allocated` - `inst-cfg-3`
   1. [ ] - `p2` - Validate sum of child allocations ≤ parent budget × `overcommit_ratio` - `inst-cfg-3a`
   2. [ ] - `p2` - **IF** validation fails - `inst-cfg-3b`
      1. [ ] - `p2` - **RETURN** 400 ValidationError with budget allocation details - `inst-cfg-3b1`
4. [ ] - `p2` - **IF** ancestor has `sharing: enforce` on rate limit - `inst-cfg-4`
   1. [ ] - `p2` - Validate descendant's limit does not exceed ancestor's enforced limit - `inst-cfg-4a`
5. [ ] - `p2` - Validate circuit breaker config: `failure_threshold` ≥ 1, `timeout_seconds` ≥ 1, `success_threshold` ≥ 1 - `inst-cfg-5`
6. [ ] - `p2` - Validate concurrency limit config: `max_concurrent` > 0, `per_tenant_max` ≤ `max_concurrent` - `inst-cfg-6`
7. [ ] - `p2` - **IF** `strategy` = `queue` on rate_limit or concurrency_limit - `inst-cfg-7`
   1. [ ] - `p2` - Validate queue config: `max_depth` (1–10,000), `timeout` (1–60s), `memory_limit` (1B–1GB), `overflow_strategy` is valid enum (`drop_newest`, `drop_oldest`, `reject`) - `inst-cfg-7a`
8. [ ] - `p2` - Persist config to database via ControlPlaneService - `inst-cfg-8`
9. [ ] - `p2` - **RETURN** updated resource - `inst-cfg-9`

## 3. Processes / Business Logic (CDSL)

### Token Bucket Rate Check

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-token-bucket-check`

**Input**: Rate limit config (algorithm, sustained rate, burst capacity, scope, cost), request context (tenant_id, user_id, IP, route_id)

**Output**: Allow (with remaining tokens) or deny (with retry-after estimate)

**Steps**:
1. [ ] - `p2` - Determine counter key from scope: `oagw:ratelimit:{scope}:{identifier}:{window}` - `inst-tb-1`
2. [ ] - `p2` - Load or create in-memory `TokenBucket` for key - `inst-tb-2`
3. [ ] - `p2` - Refill tokens: `elapsed = now - last_update`, `new_tokens = elapsed_seconds × refill_rate`, `tokens = min(tokens + new_tokens, capacity)`, `last_update = now` - `inst-tb-3`
4. [ ] - `p2` - **IF** `tokens >= cost` - `inst-tb-4`
   1. [ ] - `p2` - Deduct: `tokens -= cost` - `inst-tb-4a`
   2. [ ] - `p2` - **RETURN** Allow with `remaining = floor(tokens)`, `limit = capacity`, `reset = window_end_timestamp` - `inst-tb-4b`
5. [ ] - `p2` - **ELSE** (insufficient tokens) - `inst-tb-5`
   1. [ ] - `p2` - Calculate `retry_after = ceil((cost - tokens) / refill_rate)` - `inst-tb-5a`
   2. [ ] - `p2` - **RETURN** Deny with `retry_after`, `limit = capacity`, `remaining = 0`, `reset = window_end_timestamp` - `inst-tb-5b`

### Hierarchical Rate Limit Merge

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-hierarchical-rate-limit-merge`

**Input**: Upstream rate limit config, route rate limit config, tenant hierarchy (ancestor chain from root to current tenant)

**Output**: Effective rate limit config (sustained rate, burst capacity, scope, strategy)

**Steps**:
1. [ ] - `p2` - Collect all rate limit configs from tenant hierarchy (root → current) for the upstream - `inst-merge-1`
2. [ ] - `p2` - **FOR EACH** ancestor config from root to current - `inst-merge-2`
   1. [ ] - `p2` - **IF** ancestor sharing = `enforce` - `inst-merge-2a`
      1. [ ] - `p2` - Add to enforced limits list - `inst-merge-2a1`
   2. [ ] - `p2` - **IF** ancestor sharing = `inherit` and descendant has no own config - `inst-merge-2b`
      1. [ ] - `p2` - Use ancestor's config as base - `inst-merge-2b1`
3. [ ] - `p2` - Compute effective sustained rate: `min(own_rate, all_enforced_ancestor_rates)` - `inst-merge-3`
4. [ ] - `p2` - Compute effective burst capacity: `min(own_burst, all_enforced_ancestor_bursts)` - `inst-merge-4`
5. [ ] - `p2` - **IF** route has own rate limit config - `inst-merge-5`
   1. [ ] - `p2` - Apply route-level override: `effective = min(upstream_effective, route_config)` - `inst-merge-5a`
6. [ ] - `p2` - **RETURN** effective rate limit config - `inst-merge-6`

### Circuit Breaker State Evaluation

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-circuit-breaker-evaluation`

**Input**: Upstream circuit breaker config, upstream_id, tenant_id, event (pre-request check or post-response evaluation)

**Output**: Allow request / reject request (503 CircuitBreakerOpen) / state transition

**Steps**:
1. [ ] - `p2` - **IF** circuit breaker not enabled for upstream - `inst-cb-1`
   1. [ ] - `p2` - **RETURN** Allow (bypass circuit breaker) - `inst-cb-1a`
2. [ ] - `p2` - Load circuit state from Redis: `oagw:cb:{tenant_id}:{upstream_id}:state` - `inst-cb-2`
3. [ ] - `p2` - **IF** Redis unavailable - `inst-cb-3`
   1. [ ] - `p2` - **RETURN** Allow (fail open — graceful degradation) - `inst-cb-3a`
4. [ ] - `p2` - **IF** event = pre-request check - `inst-cb-4`
   1. [ ] - `p2` - **IF** state = CLOSED - `inst-cb-4a`
      1. [ ] - `p2` - **RETURN** Allow - `inst-cb-4a1`
   2. [ ] - `p2` - **IF** state = OPEN - `inst-cb-4b`
      1. [ ] - `p2` - Load `opened_at` timestamp from Redis - `inst-cb-4b1`
      2. [ ] - `p2` - **IF** `now - opened_at > timeout_seconds` - `inst-cb-4b2`
         1. [ ] - `p2` - Atomic transition: OPEN → HALF_OPEN (via Redis Lua script), reset `half_open_count = 0` - `inst-cb-4b2a`
         2. [ ] - `p2` - **RETURN** Allow (first probe request) - `inst-cb-4b2b`
      3. [ ] - `p2` - **ELSE** - `inst-cb-4b3`
         1. [ ] - `p2` - **IF** `fallback_strategy` = `fallback_endpoint` and `fallback_endpoint_id` configured - `inst-cb-4b3a`
            1. [ ] - `p2` - Route to fallback upstream - `inst-cb-4b3a1`
         2. [ ] - `p2` - **RETURN** Reject: 503 CircuitBreakerOpen with `retry_after = timeout_seconds - elapsed` - `inst-cb-4b3b`
   3. [ ] - `p2` - **IF** state = HALF_OPEN - `inst-cb-4c`
      1. [ ] - `p2` - Increment `half_open_count` atomically - `inst-cb-4c1`
      2. [ ] - `p2` - **IF** `half_open_count > half_open_max_requests` - `inst-cb-4c2`
         1. [ ] - `p2` - **RETURN** Reject: 503 CircuitBreakerOpen (probe limit reached) - `inst-cb-4c2a`
      3. [ ] - `p2` - **RETURN** Allow (probe request) - `inst-cb-4c3`
5. [ ] - `p2` - **IF** event = post-response evaluation - `inst-cb-5`
   1. [ ] - `p2` - Classify response as success or failure using `failure_conditions` (status codes, timeout, connection error) - `inst-cb-5a`
   2. [ ] - `p2` - **IF** failure - `inst-cb-5b`
      1. [ ] - `p2` - **IF** state = CLOSED: increment failure counter; **IF** counter ≥ `failure_threshold` → atomic transition CLOSED → OPEN, set `opened_at = now` - `inst-cb-5b1`
      2. [ ] - `p2` - **IF** state = HALF_OPEN: atomic transition HALF_OPEN → OPEN, set `opened_at = now` - `inst-cb-5b2`
   3. [ ] - `p2` - **IF** success - `inst-cb-5c`
      1. [ ] - `p2` - **IF** state = CLOSED: reset failure counter to 0 - `inst-cb-5c1`
      2. [ ] - `p2` - **IF** state = HALF_OPEN: increment success counter; **IF** counter ≥ `success_threshold` → atomic transition HALF_OPEN → CLOSED, reset all counters - `inst-cb-5c2`

### Concurrency Permit Acquisition

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-concurrency-permit-acquisition`

**Input**: Upstream concurrency config, route concurrency config, tenant-global concurrency limit, tenant_id

**Output**: Set of RAII permits (tenant + upstream + route) or rejection error

**Steps**:
1. [ ] - `p2` - **IF** tenant-global concurrency limit configured - `inst-conc-1`
   1. [ ] - `p2` - Attempt `try_acquire()` on tenant-level semaphore (key: `tenant_id`) - `inst-conc-1a`
   2. [ ] - `p2` - **IF** `in_flight >= global_concurrency_limit` - `inst-conc-1b`
      1. [ ] - `p2` - **RETURN** error: 503 ConcurrencyLimitExceeded (level: tenant) - `inst-conc-1b1`
   3. [ ] - `p2` - Acquire tenant permit (RAII — auto-released via Drop) - `inst-conc-1c`
2. [ ] - `p2` - **IF** upstream concurrency limit configured - `inst-conc-2`
   1. [ ] - `p2` - Resolve effective upstream limit via hierarchical merge: `min(ancestor.enforced, descendant)` - `inst-conc-2a`
   2. [ ] - `p2` - Attempt `try_acquire()` on upstream-level semaphore (key: `upstream_id`) - `inst-conc-2b`
   3. [ ] - `p2` - **IF** `in_flight >= effective_max_concurrent` - `inst-conc-2c`
      1. [ ] - `p2` - Release tenant permit, **RETURN** error: 503 ConcurrencyLimitExceeded (level: upstream) - `inst-conc-2c1`
   4. [ ] - `p2` - **IF** `per_tenant_max` configured and tenant's in-flight for this upstream ≥ `per_tenant_max` - `inst-conc-2d`
      1. [ ] - `p2` - Release tenant permit, **RETURN** error: 503 ConcurrencyLimitExceeded (level: upstream/per-tenant) - `inst-conc-2d1`
   5. [ ] - `p2` - Acquire upstream permit (RAII) - `inst-conc-2e`
3. [ ] - `p2` - **IF** route concurrency limit configured - `inst-conc-3`
   1. [ ] - `p2` - Attempt `try_acquire()` on route-level semaphore (key: `route_id`) - `inst-conc-3a`
   2. [ ] - `p2` - **IF** `in_flight >= route_max_concurrent` - `inst-conc-3b`
      1. [ ] - `p2` - Release upstream and tenant permits, **RETURN** error: 503 ConcurrencyLimitExceeded (level: route) - `inst-conc-3b1`
   3. [ ] - `p2` - Acquire route permit (RAII) - `inst-conc-3c`
4. [ ] - `p2` - **RETURN** permit set (all acquired permits held until request completes or errors) - `inst-conc-4`

### Backpressure Queue Management

- [ ] `p2` - **ID**: `cpt-cf-oagw-algo-backpressure-queue`

**Input**: Queued request, queue config (max_depth, timeout, ordering, memory_limit, overflow_strategy)

**Output**: Request forwarded when capacity available, or rejection error on timeout/overflow

**Steps**:
1. [ ] - `p2` - Estimate request memory: `headers_size + body_size (if buffered) + metadata_overhead (~200 bytes)` - `inst-bp-1`
2. [ ] - `p2` - **IF** `queue.total_memory + estimated_size > memory_limit` - `inst-bp-2`
   1. [ ] - `p2` - **RETURN** error: 503 QueueMemoryLimitExceeded - `inst-bp-2a`
3. [ ] - `p2` - **IF** `queue.depth >= max_depth` - `inst-bp-3`
   1. [ ] - `p2` - **IF** `overflow_strategy` = `drop_newest` - `inst-bp-3a`
      1. [ ] - `p2` - **RETURN** error: 503 QueueFull (incoming request rejected) - `inst-bp-3a1`
   2. [ ] - `p2` - **IF** `overflow_strategy` = `drop_oldest` - `inst-bp-3b`
      1. [ ] - `p2` - Dequeue oldest request, respond with 503 QueueTimeout to evicted request - `inst-bp-3b1`
      2. [ ] - `p2` - Enqueue incoming request - `inst-bp-3b2`
   3. [ ] - `p2` - **IF** `overflow_strategy` = `reject` - `inst-bp-3c`
      1. [ ] - `p2` - **RETURN** error: 503 QueueFull - `inst-bp-3c1`
4. [ ] - `p2` - Enqueue request with `enqueued_at = now`, `timeout_deadline = now + timeout` - `inst-bp-4`
5. [ ] - `p2` - Update `queue.total_memory += estimated_size` - `inst-bp-5`
6. [ ] - `p2` - Wait for permit from background queue consumer - `inst-bp-6`
7. [ ] - `p2` - **IF** `now > timeout_deadline` before permit acquired - `inst-bp-7`
   1. [ ] - `p2` - Dequeue request, update memory tracking - `inst-bp-7a`
   2. [ ] - `p2` - **RETURN** error: 503 QueueTimeout with `queue_wait_seconds` and `Retry-After` - `inst-bp-7b`
8. [ ] - `p2` - Permit acquired — continue proxy flow execution - `inst-bp-8`
9. [ ] - `p2` - **RETURN** request ready for forwarding - `inst-bp-9`

## 4. States (CDSL)

### Circuit Breaker State Machine

- [ ] `p2` - **ID**: `cpt-cf-oagw-state-circuit-breaker`

**Scope**: Per upstream (or per endpoint if `scope: per_endpoint`), per tenant

**States**:

| State | Description | Behavior |
|-------|-------------|----------|
| CLOSED | Normal operation | All requests forwarded; failure counter incremented on error; transitions to OPEN when `failure_counter >= failure_threshold` |
| OPEN | Circuit tripped | All requests immediately rejected with 503 CircuitBreakerOpen; transitions to HALF_OPEN after `timeout_seconds` elapsed since `opened_at` |
| HALF_OPEN | Recovery probe | Limited probe requests allowed (up to `half_open_max_requests` concurrent); success increments success counter; failure transitions back to OPEN; transitions to CLOSED when `success_counter >= success_threshold` |

**Transitions**:

| From | To | Trigger | Action |
|------|----|---------|--------|
| CLOSED | OPEN | `failure_counter >= failure_threshold` | Set `opened_at = now`; emit metric `oagw_circuit_breaker_transitions_total{from=CLOSED, to=OPEN}` |
| OPEN | HALF_OPEN | `now - opened_at > timeout_seconds` | Reset `half_open_count = 0`, `success_counter = 0`; emit metric `oagw_circuit_breaker_transitions_total{from=OPEN, to=HALF_OPEN}` |
| HALF_OPEN | CLOSED | `success_counter >= success_threshold` | Reset `failure_counter = 0`; emit metric `oagw_circuit_breaker_transitions_total{from=HALF_OPEN, to=CLOSED}` |
| HALF_OPEN | OPEN | Any failure during probe | Set `opened_at = now`; emit metric `oagw_circuit_breaker_transitions_total{from=HALF_OPEN, to=OPEN}` |

**Storage**: Redis keys per `cpt-cf-oagw-adr-circuit-breaker`:
- `oagw:cb:{tenant_id}:{upstream_id}:state` → `CLOSED` | `OPEN` | `HALF_OPEN`
- `oagw:cb:{tenant_id}:{upstream_id}:failures` → counter (TTL: rolling window)
- `oagw:cb:{tenant_id}:{upstream_id}:opened_at` → timestamp
- `oagw:cb:{tenant_id}:{upstream_id}:half_open_count` → counter

**Graceful degradation**: If Redis unavailable, default to CLOSED state (fail open).

## 5. Definitions of Done

### Implement Token Bucket Rate Limiter

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-token-bucket`

The system **MUST** implement token bucket rate limiting with dual-rate configuration (sustained rate + burst capacity) per `cpt-cf-oagw-adr-rate-limiting`. Tokens **MUST** be refilled at `sustained.rate` per `sustained.window`. Burst **MUST** be capped at `burst.capacity`. Each request **MUST** consume `cost` tokens (default: 1). Counter scope **MUST** support: `global`, `tenant`, `user`, `ip`, `route`. When tokens insufficient and strategy = `reject`, the system **MUST** return 429 RateLimitExceeded with `Retry-After` header.

**Implements**:
- `cpt-cf-oagw-flow-rate-limited-proxy`
- `cpt-cf-oagw-algo-token-bucket-check`

**Touches**:
- Entities: `Upstream` (rate_limit field), `Route` (rate_limit field)

### Implement Rate Limit Response Headers

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-rate-limit-headers`

The system **MUST** include `X-RateLimit-Limit`, `X-RateLimit-Remaining`, and `X-RateLimit-Reset` response headers on all proxy responses when `response_headers: true` in rate limit config (default: true) per RFC 6585 / draft-ietf-httpapi-ratelimit-headers. On 429 responses, the system **MUST** include `Retry-After` header with the calculated wait time in seconds.

**Implements**:
- `cpt-cf-oagw-flow-rate-limited-proxy`

**Touches**:
- API: `{METHOD} /api/oagw/v1/proxy/{alias}/{path}`

### Implement Hierarchical Rate Limit Merge

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-hierarchical-rate-merge`

The system **MUST** compute effective rate limits by walking the tenant hierarchy and applying `min(ancestor.enforced, descendant)` per `cpt-cf-oagw-adr-rate-limiting`. When ancestor sharing = `enforce`, descendant **MUST NOT** exceed ancestor's limit. When sharing = `inherit` and descendant has no config, ancestor's config **MUST** be used. Budget allocation **MUST** validate that sum of child allocations ≤ parent budget × `overcommit_ratio`. Route-level rate limits **MUST** apply as `min(upstream_effective, route_config)`.

**Implements**:
- `cpt-cf-oagw-algo-hierarchical-rate-limit-merge`
- `cpt-cf-oagw-flow-configure-rate-limits`

**Touches**:
- Entities: `Upstream` (rate_limit_sharing, rate_limit.budget)

### Implement Circuit Breaker State Machine

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-circuit-breaker`

The system **MUST** implement the circuit breaker state machine (CLOSED → OPEN → HALF_OPEN → CLOSED) per `cpt-cf-oagw-adr-circuit-breaker`. State **MUST** be stored in Redis with atomic transitions via Lua scripts. Failure conditions **MUST** be configurable: HTTP status codes (default: 500, 502, 503, 504), timeouts, connection errors. When OPEN, the system **MUST** return 503 CircuitBreakerOpen with `Retry-After` and `X-Circuit-State` headers. When Redis is unavailable, the system **MUST** default to CLOSED (fail open).

**Implements**:
- `cpt-cf-oagw-flow-rate-limited-proxy`
- `cpt-cf-oagw-algo-circuit-breaker-evaluation`
- `cpt-cf-oagw-state-circuit-breaker`

**Touches**:
- Entities: `Upstream` (circuit_breaker field)

### Implement Concurrency Control

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-concurrency-control`

The system **MUST** implement semaphore-based in-memory concurrency limiting at three levels: tenant-global, upstream, and route per `cpt-cf-oagw-adr-concurrency-control`. Permits **MUST** use RAII pattern (auto-released via Drop on completion, error, or timeout). A request **MUST** pass all applicable concurrency checks: `[Tenant Limit] → [Upstream Limit] → [Route Limit]`. Streaming requests **MUST** hold permits until stream completes or client disconnects. Per-tenant fairness **MUST** be enforced via `per_tenant_max` on upstream concurrency config. When limit exceeded and strategy = `reject`, the system **MUST** return 503 ConcurrencyLimitExceeded with `Retry-After`.

**Implements**:
- `cpt-cf-oagw-flow-rate-limited-proxy`
- `cpt-cf-oagw-algo-concurrency-permit-acquisition`

**Touches**:
- Entities: `Upstream` (concurrency_limit field), `Route` (concurrency_limit field)
- DB: `oagw_upstream` (concurrency_limit column), `oagw_route` (concurrency_limit column)

### Implement Backpressure Queueing

- [ ] `p2` - **ID**: `cpt-cf-oagw-dod-backpressure-queue`

The system **MUST** implement `reject` and `queue` strategies for handling overload per `cpt-cf-oagw-adr-backpressure-queueing`. Queue strategy **MUST** support: FIFO ordering, configurable `max_depth` (1–10,000), `timeout` (1–60s), `memory_limit` (1B–1GB), and `overflow_strategy` (`drop_newest`, `drop_oldest`, `reject`). Queue consumer **MUST** dequeue and execute requests when permits become available. When circuit breaker is OPEN, the queue **MUST NOT** accumulate requests (immediate rejection). All backpressure responses **MUST** include `Retry-After` header.

**Implements**:
- `cpt-cf-oagw-flow-rate-limited-proxy`
- `cpt-cf-oagw-algo-backpressure-queue`

**Touches**:
- Entities: `Upstream` (concurrency_limit.strategy), `Route` (concurrency_limit.strategy)

## 6. Acceptance Criteria

- [ ] Token bucket rate limiter allows requests when tokens available and rejects with 429 when tokens exhausted
- [ ] Rate limit config supports dual-rate: `sustained` (rate + window) and `burst` (capacity) independently
- [ ] Counter scopes (`global`, `tenant`, `user`, `ip`, `route`) track and enforce limits independently
- [ ] Cost-based rate limiting deducts `cost` tokens per request (configurable per route)
- [ ] `X-RateLimit-Limit`, `X-RateLimit-Remaining`, `X-RateLimit-Reset` headers included in all proxy responses when `response_headers: true`
- [ ] 429 RateLimitExceeded responses include `Retry-After` header with calculated wait time
- [ ] Hierarchical rate limit merge computes `effective = min(ancestor.enforced, descendant)` across tenant hierarchy
- [ ] Ancestor with `sharing: enforce` prevents descendant from exceeding enforced limit
- [ ] Ancestor with `sharing: inherit` provides default config when descendant has none
- [ ] Budget allocation validates sum of child allocations ≤ parent budget × `overcommit_ratio`; rejects if exceeded
- [ ] Route-level rate limit applies as `min(upstream_effective, route_config)`
- [ ] Circuit breaker transitions CLOSED → OPEN when consecutive failures ≥ `failure_threshold`
- [ ] Circuit breaker transitions OPEN → HALF_OPEN after `timeout_seconds` elapsed
- [ ] Circuit breaker transitions HALF_OPEN → CLOSED after `success_threshold` consecutive probe successes
- [ ] Circuit breaker transitions HALF_OPEN → OPEN on any probe failure
- [ ] Circuit breaker OPEN state returns 503 CircuitBreakerOpen with `Retry-After` and `X-Circuit-State: OPEN` headers
- [ ] Circuit breaker HALF_OPEN limits concurrent probes to `half_open_max_requests`
- [ ] Circuit breaker failure conditions configurable: status codes, timeout, connection error
- [ ] Circuit breaker state stored in Redis with atomic transitions via Lua scripts
- [ ] Circuit breaker defaults to CLOSED (fail open) when Redis unavailable
- [ ] Concurrency limiting enforces three independent levels: tenant-global, upstream, route
- [ ] Concurrency permits use RAII pattern — auto-released on completion, error, timeout, or panic
- [ ] Per-tenant fairness enforced via `per_tenant_max` on upstream concurrency config
- [ ] Streaming requests hold concurrency permits until stream completes or client disconnects
- [ ] Concurrency limit exceeded returns 503 ConcurrencyLimitExceeded with `Retry-After`
- [ ] Backpressure queue strategy enqueues requests when capacity unavailable and dequeues when permits released
- [ ] Queue respects `max_depth`, `timeout`, `memory_limit` bounds
- [ ] Queue overflow handled per `overflow_strategy`: `drop_newest`, `drop_oldest`, or `reject`
- [ ] Queue timeout returns 503 QueueTimeout with `queue_wait_seconds` and `Retry-After`
- [ ] Queue does not accumulate requests when circuit breaker is OPEN
- [ ] All gateway-originated errors include `X-OAGW-Error-Source: gateway` and use RFC 9457 Problem Details format
- [ ] Prometheus metrics emitted: `oagw_rate_limit_exceeded_total`, `oagw_rate_limit_usage_ratio`, `oagw_circuit_breaker_state`, `oagw_circuit_breaker_transitions_total`, `oagw_requests_in_flight`, `oagw_concurrency_limit_exceeded_total`, `oagw_queue_depth`, `oagw_queue_wait_duration_seconds`
- [ ] Rate limit check latency < 1ms (in-memory token bucket) per `cpt-cf-oagw-nfr-low-latency`
- [ ] No credentials or PII appear in rate limit / circuit breaker error responses or logs

## 7. Additional Context

### Performance Considerations

Rate limiting and concurrency control are on the hot path for every proxy request. Token bucket check is an in-memory operation (<1ms per `cpt-cf-oagw-nfr-low-latency`). Concurrency permit acquisition uses `AtomicUsize` operations (<100ns per check per `cpt-cf-oagw-adr-concurrency-control`). Circuit breaker state check requires a Redis round-trip (~1-2ms) but can fall back to local-only (fail open) if Redis is unavailable. Redis sync for distributed rate limiting uses a configurable interval (default 100ms) to balance accuracy vs load.

### Distributed State

MVP uses per-instance rate limiting and concurrency control (no distributed coordination). Distributed rate limiting via Redis (hybrid local + periodic sync per `cpt-cf-oagw-adr-rate-limiting`) and distributed circuit breaker state (Redis-backed per `cpt-cf-oagw-adr-circuit-breaker`) are production-grade capabilities. Redis key structure: `oagw:ratelimit:{scope}:{identifier}:{window}` for rate limits, `oagw:cb:{tenant_id}:{upstream_id}:*` for circuit breaker.

### Deliberate Omissions

- **Custom rate limit algorithms**: Only token bucket (default) and sliding window (optional) per `cpt-cf-oagw-adr-rate-limiting`. Custom algorithms are out of scope.
- **Dynamic rate limit adjustment via API**: Future work — not addressed in this feature.
- **Request priority queueing**: Future enhancement per `cpt-cf-oagw-adr-backpressure-queueing` Phase 3. Only FIFO ordering in this feature.
- **Degrade strategy**: Deferred to Phase 3-4 per `cpt-cf-oagw-adr-backpressure-queueing`. Only `reject` and `queue` strategies implemented.
- **Health check based proactive detection**: Not included — circuit breaker is reactive only per `cpt-cf-oagw-adr-circuit-breaker`.
- **Structured audit logging**: Rate limit exceeded and circuit breaker state transition events are logged per DESIGN.md §4.3 but structured audit logging integration belongs to `cpt-cf-oagw-feature-observability`.
- **UX/Accessibility**: Not applicable — OAGW is a backend API module with no user interface.
- **Compliance/Privacy**: OAGW does not handle PII directly. No additional regulatory compliance beyond standard platform requirements.
