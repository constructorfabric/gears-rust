---
status: proposed
date: 2026-02-03
decision-makers: OAGW Team
---

# Circuit Breaker — Core Gateway Functionality with Redis-Based Distributed State


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [State Machine](#state-machine)
  - [Configuration](#configuration)
  - [Distributed State (Redis)](#distributed-state-redis)
  - [Fallback Strategies](#fallback-strategies)
  - [Integration with Error Handling](#integration-with-error-handling)
  - [Error Response](#error-response)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Circuit breaker as core functionality](#circuit-breaker-as-core-functionality)
  - [Circuit breaker as plugin](#circuit-breaker-as-plugin)
  - [No circuit breaker (client responsibility)](#no-circuit-breaker-client-responsibility)
  - [Health check based (proactive)](#health-check-based-proactive)
- [More Information](#more-information)
  - [Hierarchical Configuration](#hierarchical-configuration)
  - [Observability and Metrics](#observability-and-metrics)
  - [Implementation Notes](#implementation-notes)
  - [References](#references)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-circuit-breaker`

## Context and Problem Statement

OAGW needs a circuit breaker mechanism to prevent cascading failures when upstream services become unhealthy. When an upstream experiences persistent failures (timeouts, 5xx errors, connection issues), continuing to send requests wastes resources, increases latency, can worsen upstream condition, and lacks fast-fail behavior. Circuit breaker is core functionality (not a plugin) because it requires distributed state coordination, atomic transitions, and deep integration with error handling and routing logic.

## Decision Drivers

* Fast failure detection (seconds, not minutes)
* Automatic recovery without manual intervention
* Minimal false positives during transient errors
* Distributed coordination of state across OAGW nodes
* Per-upstream isolation (one upstream's failure doesn't affect others)
* Observability with clear metrics and state visibility
* Graceful degradation with fallback strategies

## Considered Options

* Circuit breaker as core gateway functionality with Redis-based state
* Circuit breaker as plugin
* No circuit breaker (client responsibility)
* Health check based (proactive) instead of circuit breaker (reactive)

## Decision Outcome

Chosen option: "Circuit breaker as core gateway functionality with Redis-based distributed state", because it provides strong consistency, fast failure detection, and tight integration with routing logic.

### State Machine

```text
     CLOSED ──(failure_threshold reached)──► OPEN
        ▲                                      │
        │                              (timeout_seconds)
        │                                      ▼
        └──(success_threshold reached)── HALF-OPEN
                                               │
                              (failure)────────┘
```

- **CLOSED**: Normal operation, failure counter increments on errors
- **OPEN**: All requests rejected immediately with `503 CircuitBreakerOpen`
- **HALF-OPEN**: Limited probe requests test recovery; success → CLOSED, failure → OPEN

### Configuration

Circuit breaker is a first-class field in upstream definitions (not a plugin).

#### Schema

```json
{
  "circuit_breaker": {
    "type": "object",
    "properties": {
      "enabled": {
        "type": "boolean",
        "default": true,
        "description": "Enable/disable circuit breaker for this upstream"
      },
      "failure_threshold": {
        "type": "integer",
        "minimum": 1,
        "default": 5,
        "description": "Consecutive failures before opening circuit"
      },
      "success_threshold": {
        "type": "integer",
        "minimum": 1,
        "default": 3,
        "description": "Consecutive successes in half-open before closing circuit"
      },
      "timeout_seconds": {
        "type": "integer",
        "minimum": 1,
        "default": 30,
        "description": "Seconds circuit stays open before entering half-open"
      },
      "half_open_max_requests": {
        "type": "integer",
        "minimum": 1,
        "default": 3,
        "description": "Max concurrent requests allowed in half-open state"
      },
      "failure_conditions": {
        "type": "object",
        "properties": {
          "status_codes": {
            "type": "array",
            "items": { "type": "integer" },
            "default": [ 500, 502, 503, 504 ],
            "description": "HTTP status codes counted as failures"
          },
          "timeout": {
            "type": "boolean",
            "default": true,
            "description": "Count request timeouts as failures"
          },
          "connection_error": {
            "type": "boolean",
            "default": true,
            "description": "Count connection errors as failures"
          }
        }
      },
      "scope": {
        "type": "string",
        "enum": [ "global", "per_endpoint" ],
        "default": "global",
        "description": "Circuit breaker scope: global for entire upstream or per individual endpoint"
      },
      "fallback_strategy": {
        "type": "string",
        "enum": [ "fail_fast", "fallback_endpoint", "cached_response" ],
        "default": "fail_fast",
        "description": "Behavior when circuit is open"
      },
      "fallback_endpoint_id": {
        "type": "string",
        "format": "uuid",
        "description": "Fallback upstream ID when strategy is fallback_endpoint"
      }
    }
  }
}
```

#### Example

```json
{
  "server": {
    "endpoints": [
      { "scheme": "https", "host": "api.openai.com", "port": 443 }
    ]
  },
  "protocol": "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1",
  "circuit_breaker": {
    "enabled": true,
    "failure_threshold": 5,
    "success_threshold": 3,
    "timeout_seconds": 30,
    "half_open_max_requests": 3,
    "failure_conditions": {
      "status_codes": [500, 502, 503, 504],
      "timeout": true,
      "connection_error": true
    },
    "scope": "global",
    "fallback_strategy": "fail_fast"
  }
}
```

### Distributed State (Redis)

State keys:

```text
oagw:cb:{tenant_id}:{upstream_id}:state        → "CLOSED" | "OPEN" | "HALF_OPEN"
oagw:cb:{tenant_id}:{upstream_id}:failures     → counter (TTL: rolling window)
oagw:cb:{tenant_id}:{upstream_id}:opened_at    → timestamp
oagw:cb:{tenant_id}:{upstream_id}:half_open_count → counter for concurrent half-open requests
```

Atomic state checking via Lua script:

```lua
-- Check circuit state (fast path)
local state = redis.call('GET', state_key)
if state == 'OPEN' then
    local opened_at = redis.call('GET', opened_at_key)
    if (now - opened_at) > timeout_seconds then
        -- Transition to HALF_OPEN
        redis.call('SET', state_key, 'HALF_OPEN')
        redis.call('SET', half_open_count_key, 0)
        return 'HALF_OPEN'
    else
        return 'OPEN'
    end
end
return state or 'CLOSED'
```

**Pros**: Strong consistency across nodes, atomic operations via Lua scripts, fast (<1ms latency), supports distributed counters.
**Cons**: Dependency on Redis, single point of failure (mitigated by Redis HA).

**Alternative: Eventually Consistent In-Memory State** — Each OAGW node maintains local circuit state with gossip-based state changes via pub/sub. Rejected due to state divergence, delayed failure detection, and complex coordination logic.

Graceful degradation: if Redis unavailable, default to CLOSED state (fail open).

### Fallback Strategies

When circuit is **OPEN**, OAGW can respond in different ways:

- **fail_fast** (default): Immediately return `503 CircuitBreakerOpen` without calling upstream. Client can handle error and retry with backoff. Latency: <1ms (no network call).
- **fallback_endpoint**: Route request to alternative upstream (`fallback_endpoint_id` required). Use cases: multi-region deployments (primary: us-east, fallback: us-west), backup service providers (primary: OpenAI, fallback: Azure OpenAI). Fallback upstream must be API-compatible. Latency: normal request latency + routing overhead.
- **cached_response**: Return last successful response from cache (if available). Use case: read-only APIs where stale data is acceptable (config, metadata). Only for idempotent GET requests. Latency: <10ms (cache lookup).

  > **Cache-key scoping (mandatory).** Because OAGW is multi-tenant, every
  > cached response **must** be keyed by all of the following components to
  > prevent cross-tenant and cross-principal data leakage:
  >
  > | Component | Source |
  > |---|---|
  > | `tenant_id` | Security context of the inbound request |
  > | `upstream_id` | Resolved upstream for the route |
  > | `route_id` | Matched route definition |
  > | `principal_id` (or token fingerprint) | Authenticated caller identity |
  > | HTTP `Vary` header fields | Values of each header listed in the upstream response's `Vary` header |
  >
  > Implementations **must** honor HTTP `Vary` semantics: if the upstream
  > response includes a `Vary` header, the indicated request-header values
  > become additional cache-key components. A `Vary: *` response is
  > never cacheable.
  >
  > **Route-level response caching is disabled by default.** The
  > `cached_response` strategy is only active when the route explicitly
  > configures a valid `response_cache` policy (including cache-key
  > components and a TTL). Without such a policy the gateway treats the
  > strategy as `fail_fast` and returns `503`.
  >
  > **Design-principle constraint:** OAGW's baseline stance is "no response
  > caching" (`cpt-cf-oagw-principle-no-cache`). The `cached_response`
  > fallback is a narrow, opt-in exception limited to circuit-breaker
  > open state; it does **not** enable general-purpose response caching.

### Integration with Error Handling

Circuit breaker evaluates responses and updates state:

```rust
async fn handle_upstream_response(
    response: Result<UpstreamResponse, UpstreamError>,
    circuit: &CircuitBreaker,
) -> Result<Response> {
    let is_failure = match &response {
        Ok(resp) if circuit.config.failure_conditions.status_codes.contains(&resp.status) => true,
        Ok(_) => false,
        Err(UpstreamError::Timeout) if circuit.config.failure_conditions.timeout => true,
        Err(UpstreamError::Connection) if circuit.config.failure_conditions.connection_error => true,
        Err(_) => false,
    };

    if is_failure {
        circuit.record_failure().await?;
    } else {
        circuit.record_success().await?;
    }

    response.map(|r| r.into())
}
```

### Error Response

When circuit is open:

```json
{
  "error": {
    "type": "gts.x.core.errors.err.v1~x.oagw.circuit_breaker.open.v1",
    "status": 503,
    "code": "CIRCUIT_BREAKER_OPEN",
    "message": "Circuit breaker is open for upstream api.openai.com",
    "details": {
      "upstream_id": "uuid",
      "state": "OPEN",
      "opened_at": "2026-02-03T10:45:00Z",
      "retry_after_seconds": 15
    }
  }
}
```

Headers:

```http
Retry-After: 15
X-Circuit-State: OPEN
```

### Consequences

* Good, because fast failure detection and automatic recovery
* Good, because prevents cascading failures
* Good, because reduces wasted resources on unhealthy upstreams
* Good, because better UX (fast 503 vs long timeout)
* Bad, because adds complexity to request handling path
* Bad, because dependency on Redis for distributed state
* Bad, because false positives possible during upstream maintenance
* Bad, because additional monitoring/alerting needed
* Neutral, circuit breaker state is shared globally per upstream (not per-route)
* Neutral, manual intervention needed to override automatic behavior
* Neutral, requires careful tuning of thresholds per upstream

### Confirmation

Integration tests verify: state transitions (CLOSED→OPEN→HALF-OPEN→CLOSED), failure threshold counting, timeout-based recovery, and 503 response with `Retry-After` and `X-Circuit-State` headers.

## Pros and Cons of the Options

### Circuit breaker as core functionality

* Good, because tight integration with routing and error handling
* Good, because consistent behavior for all upstreams
* Good, because access to distributed state coordination
* Bad, because adds core complexity

### Circuit breaker as plugin

* Good, because optional per-upstream
* Bad, because plugins cannot access distributed state efficiently
* Bad, because plugin ordering issues with other guards

### No circuit breaker (client responsibility)

* Good, because simplest OAGW implementation
* Bad, because clients may not implement proper circuit breaking
* Bad, because OAGW is better positioned to detect upstream health across all tenants

### Health check based (proactive)

* Good, because faster recovery detection
* Bad, because requires health check endpoint on all upstreams
* Neutral, can be combined with circuit breaker for best results

## More Information

### Hierarchical Configuration

Circuit breaker configuration follows same inheritance rules as rate limits:

```json
{
  "circuit_breaker": {
    "sharing": "inherit",
    "enabled": true,
    "failure_threshold": 10,
    "timeout_seconds": 60
  }
}
```

- **private**: Descendant tenants define their own circuit breaker config
- **inherit**: Descendant can override if needed
- **enforce**: Descendant must use ancestor's config (cannot disable or weaken thresholds)

### Observability and Metrics

```promql
oagw_circuit_breaker_state{upstream_id, tenant_id} → 0=CLOSED, 1=HALF_OPEN, 2=OPEN
oagw_circuit_breaker_failures_total{upstream_id, tenant_id}
oagw_circuit_breaker_state_changes_total{upstream_id, tenant_id, from_state, to_state}
oagw_circuit_breaker_rejected_requests_total{upstream_id, tenant_id}
oagw_circuit_breaker_half_open_successes_total{upstream_id, tenant_id}
oagw_circuit_breaker_half_open_failures_total{upstream_id, tenant_id}
```

### Implementation Notes

1. **Atomic state transitions**: Use Redis WATCH/MULTI/EXEC or Lua scripts for atomic state changes
2. **Graceful degradation**: If Redis unavailable, default to CLOSED state (fail open)
3. **Per-endpoint granularity**: When `scope: per_endpoint`, maintain separate circuit state for each endpoint in upstream
4. **Manual override**: Admin API to manually open/close circuits for maintenance
5. **Warm-up period**: After deployment, circuit starts in CLOSED with reduced sensitivity

- [ADR: Rate Limiting](./0004-rate-limiting.md) — Shares distributed state infrastructure
- [ADR: Error Source Distinction](./0013-error-source-distinction.md) — Circuit breaker errors must be distinguishable from upstream errors

### References

- [Netflix Hystrix](https://github.com/Netflix/Hystrix/wiki/How-it-Works)
- [Martin Fowler: Circuit Breaker](https://martinfowler.com/bliki/CircuitBreaker.html)
- [AWS: Circuit Breaker Pattern](https://aws.amazon.com/builders-library/using-circuit-breakers-to-protect-services/)
- [Envoy Circuit Breaking](https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/upstream/circuit_breaking)

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-nfr-high-availability` — Circuit breakers prevent cascade failures, maintain 99.9% availability
* `cpt-cf-oagw-fr-enable-disable` — Emergency circuit break at management layer
* `cpt-cf-oagw-usecase-proxy-request` — Circuit breaker check before upstream call
