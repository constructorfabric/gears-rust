---
status: proposed
date: 2026-02-03
decision-makers: Constructor Fabric Steering Committee
---

# Circuit Breaker — Core Gateway Functionality with Redis-Based Distributed State

> **Implementation status:** the circuit breaker is implemented as core functionality, without Redis: each replica keeps its breakers in memory (`toolkit-resilience`), one per upstream (and config owner) shared by every tenant, and opens on the failures it sees. The upstream field has `sharing`, `enabled`, `failure_threshold` (1–1000), `timeout_seconds` (1–86400) and `success_threshold` (1–100, which also bounds the concurrent half-open probes); failures are fixed and limited to those the upstream caused (see [Breaker Scope and Failure Classification](#breaker-scope-and-failure-classification), which amends the per-tenant state keys below). `half_open_max_requests`, `failure_conditions`, `scope: per_endpoint`, fallback strategies, the `X-Circuit-State` header and the `oagw_circuit_breaker_*` metrics below are not implemented; the metrics are the toolkit's `resilience.circuit_breaker.*`. See F0006 [Circuit Breaker State Evaluation](../features/0006-cpt-cf-oagw-feature-rate-limiting.md#circuit-breaker-state-evaluation) and DESIGN *Circuit Breaker*.


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [State Machine](#state-machine)
  - [Breaker Scope and Failure Classification](#breaker-scope-and-failure-classification)
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

### Breaker Scope and Failure Classification

*Amended 2026-11-20 with the implementation (#4974). It replaces the per-tenant state keys of [Distributed State (Redis)](#distributed-state-redis).*

#### One breaker per upstream, shared by every tenant

A breaker belongs to an upstream (and to the upstream whose level set the effective config, so a tenant that resolves a different `private` or inherited config on the same upstream gets its own). The requesting tenant is not part of the key: every tenant that resolves to the upstream shares its breaker on a replica.

- The breaker answers one question, whether the upstream can serve now, and the answer is the same for every caller. Per-tenant breakers would make each tenant find an outage on its own, `failure_threshold` failed requests each, and keep sending traffic to an upstream that is down.
- Established proxies key the same way. [Envoy outlier detection](https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/upstream/outlier) tracks the upstream host, shared by all downstream callers; `consecutive_gateway_failure` counts `502`/`503`/`504`; with `split_external_local_origin_errors` the locally originated errors (connect failure, timeout, reset) are tracked apart so the detector can ignore them; `max_ejection_percent` limits the impact. [Istio `DestinationRule` `outlierDetection`](https://istio.io/latest/docs/reference/config/networking/destination-rule/#OutlierDetection) is per host: `consecutiveGatewayErrors` counts `502`/`503`/`504` only, `consecutive5xxErrors` every `5xx`, and `4xx` never counts. [resilience4j](https://resilience4j.readme.io/docs/circuitbreaker), [Hystrix](https://github.com/Netflix/Hystrix/wiki/How-it-Works) and [Polly](https://www.pollydocs.org/strategies/circuit-breaker.html) keep one breaker per downstream dependency (a named instance), and the application decides which outcomes count (`recordExceptions` / `ignoreExceptions`).
- Per-caller breakers fit only when each caller has a dependency of its own. On a shared dependency, keeping one tenant from crowding out the others is the job of rate limits and bulkheads; OAGW has per-tenant rate limits ([ADR-0004](./0004-rate-limiting.md)).

#### Only failures the upstream caused count

A shared breaker must not open because of what one caller did. So, like Envoy with `split_external_local_origin_errors`, OAGW tells what the upstream did apart from what happened on the gateway's side, and counts only the former:

| Outcome | Breaker |
|---|---|
| Upstream `502`, `503`, `504` | failure |
| Any other upstream status (`500`, `4xx`, `101` included) | success |
| DNS failure or no address, connection refused, no route, connect error or timeout, TLS handshake or certificate error, peer closed, invalid response, HTTP/2 or stream error before the response head, the engine's own read or write timeout | failure |
| Request deadline hit during target resolution or the exchange, which started with at least 90 % of the request timeout left | failure |
| The same, when the gateway's own steps before them and an auth plugin's signing phase together used more than 10 % of it | not counted |
| A timeout or a broken exchange while the client is still sending a streamed request body (a failure to connect still counts) | not counted |
| Any outcome of a request pinned with `X-OAGW-Target-Host` to one endpoint of an upstream with several | not counted |
| The SSRF policy refuses every resolved address | not counted |
| Gateway policy, validation, CORS, auth plugin, guard, rate-limit, CredStore, body-limit or internal errors | not counted |
| The client cancels before the response head | not counted |
| Anything after the response head (SSE or long body, WebSocket tunnel) | not counted |

*Full budget.* The gateway's request timeout (`proxy_timeout_secs`) covers the `request_ref` CredStore read ([ADR-0022](./0022-request-secret-refs.md)), endpoint selection, target resolution and the exchange together. The permit that counts the outcome is taken just before target resolution and notes how much of the timeout is left. An auth plugin's signing phase ([ADR-0023](./0023-request-signing.md)) runs after target resolution and may read CredStore too (the AWS SigV4 plugin does); its time is taken off what the permit noted once it ends. A deadline hit counts only when the gateway's steps, signing included, used at most a tenth: a slow CredStore read must not open the breakers of healthy upstreams. A separate deadline for the exchange was rejected because ADR-0022 bounds a `request_ref` request by one timeout.

*Client uploads.* Until the client has sent its whole streamed body, the exchange runs at the client's pace: a client that trickles its upload hits the gateway's deadline or the engine's read timeout, or makes the upstream give up on it. Counted, that would let any caller open the circuit for every tenant with a few slow uploads. While the body is still coming, only a failure to connect (refused, no route, connect or TLS timeout, TLS or certificate error) counts; nothing of the request was sent then, so the client's pace played no part.

*Pinned endpoints.* `X-OAGW-Target-Host` lets the caller pick one endpoint of several, and the breaker covers the whole upstream. A caller pinning a dead endpoint would open the circuit for every tenant while the other endpoints serve, so a pinned request on a multi-endpoint upstream counts as nothing; it is still rejected while the circuit is open. On a single-endpoint upstream the header picks the only endpoint and changes nothing. Per-endpoint breakers (below) would count these requests against their endpoint.

*What a caller can still do.* The table keeps a caller's own errors out, not the upstream's answers to what the caller asks for. A path that answers `503`, or a request the upstream takes longer than the timeout to answer (a large non-streaming LLM completion), counts against the upstream, so a tenant with access to a shared upstream can open its breaker with `failure_threshold` such requests in a row. Envoy, Istio and resilience4j behave the same; they assume callers that do not attack each other. Mitigations: keep `failure_threshold` above what one caller sends in a burst, keep the rate limit per tenant, and use the failure rate over a window with a minimum number of calls (future work below), which one tenant's burst moves less while others succeed.

*Client cancellation.* A request the client abandons before the response head records nothing and frees its half-open slot, also when the upstream was slow: when to give up is the caller's choice. A client that cancels before the gateway's timeout therefore never adds a timeout failure; the gateway's own timeout does.

*Admission.* A check before the auth plugin, the rate limits and the CredStore read fails fast while the circuit is open, so an open circuit costs no credential or token; it keeps no probe slot. The permit, which holds a half-open probe slot, is taken just before target resolution, so the gateway's own steps before it never hold one; only an auth plugin's signing phase, which runs between target resolution and the exchange, does. The cost is at the open → half-open edge: requests that pass the check before the first probe takes its slot do the gateway's steps and get `503` when they ask for the permit. This lasts at most the time between the check and the permit of the first request after `timeout_seconds`, once per open period; once a probe holds the slot, the check rejects.

*Resets.* Each replica rebuilds a breaker closed on the next request when the effective config, the alias or the endpoints differ from those it was built for. A request that resolves no config leaves the breaker alone: under a `private` config the owner and a descendant resolving to the same upstream see different configs, and a descendant's request must not drop the owner's breaker. So the write path drops the upstream's breakers when the stored `circuit_breaker` changes, removal included, and on a delete, on the replica that takes the write; an idle sweep removes breakers no request uses. Another replica does not see a removal: an identical config re-added there before the sweep continues the old state, which is open for at most `timeout_seconds`. While two snapshots of one upstream are both in use (proxy config cache lag, a write in progress), each switch between them rebuilds the breaker closed, so it may not open until the snapshots converge; this is transient.

#### Future work

- **Per-endpoint scope** (`scope: per_endpoint` in the schema below): one breaker per endpoint of a pool, like Envoy's host ejection, so a failing endpoint is taken out while the others serve, with a cap like `max_ejection_percent`.
- **Failure rate over a sliding window with a minimum number of calls** (resilience4j `failureRateThreshold`, `slidingWindowSize`, `minimumNumberOfCalls`) instead of consecutive failures, which a busy upstream with occasional errors trips less readily, and which one tenant's burst of failing requests moves less while the others succeed. `toolkit-resilience` already supports it (`failure_ratio`, `minimum_calls`, `window`).

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
        "maximum": 1000,
        "default": 5,
        "description": "Consecutive failures before opening circuit"
      },
      "success_threshold": {
        "type": "integer",
        "minimum": 1,
        "maximum": 100,
        "default": 3,
        "description": "Consecutive successes in half-open before closing circuit"
      },
      "timeout_seconds": {
        "type": "integer",
        "minimum": 1,
        "maximum": 86400,
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
            "default": [ 502, 503, 504 ],
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
  "protocol": "gts.cf.core.oagw.protocol.v1~cf.core.oagw.http.v1",
  "circuit_breaker": {
    "enabled": true,
    "failure_threshold": 5,
    "success_threshold": 3,
    "timeout_seconds": 30,
    "half_open_max_requests": 3,
    "failure_conditions": {
      "status_codes": [502, 503, 504],
      "timeout": true,
      "connection_error": true
    },
    "scope": "global",
    "fallback_strategy": "fail_fast"
  }
}
```

### Distributed State (Redis)

State keys (per upstream and config owner, shared by every tenant; see [Breaker Scope and Failure Classification](#breaker-scope-and-failure-classification)):

```text
oagw:cb:{upstream_id}:{owner_id}:state        → "CLOSED" | "OPEN" | "HALF_OPEN"
oagw:cb:{upstream_id}:{owner_id}:failures     → counter (TTL: rolling window)
oagw:cb:{upstream_id}:{owner_id}:opened_at    → timestamp
oagw:cb:{upstream_id}:{owner_id}:half_open_count → counter for concurrent half-open requests
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

Circuit breaker evaluates responses and updates state (the original sketch; the implemented classification, which counts only failures the upstream caused, is in [Breaker Scope and Failure Classification](#breaker-scope-and-failure-classification)):

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
    "type": "gts.cf.core.errors.err.v1~cf.oagw.circuit_breaker.open.v1",
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
* Neutral, circuit breaker state is shared per upstream by every tenant and route; tenant isolation comes from rate limits
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
oagw_circuit_breaker_state{upstream_id} → 0=CLOSED, 1=HALF_OPEN, 2=OPEN
oagw_circuit_breaker_failures_total{upstream_id}
oagw_circuit_breaker_state_changes_total{upstream_id, from_state, to_state}
oagw_circuit_breaker_rejected_requests_total{upstream_id}
oagw_circuit_breaker_half_open_successes_total{upstream_id}
oagw_circuit_breaker_half_open_failures_total{upstream_id}
```

Implemented instead: `resilience.circuit_breaker.{calls,state,transitions}` with `resilience.name` = `{alias}/{upstream_id}` (and `/{owner_id}` when the config is another upstream's).

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
- [Envoy Outlier Detection](https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/upstream/outlier)
- [Istio DestinationRule: OutlierDetection](https://istio.io/latest/docs/reference/config/networking/destination-rule/#OutlierDetection)
- [resilience4j CircuitBreaker](https://resilience4j.readme.io/docs/circuitbreaker)
- [Polly circuit breaker](https://www.pollydocs.org/strategies/circuit-breaker.html)

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-nfr-high-availability` — Circuit breakers prevent cascade failures, maintain 99.9% availability
* `cpt-cf-oagw-fr-enable-disable` — Emergency circuit break at management layer
* `cpt-cf-oagw-usecase-proxy-request` — Circuit breaker check before upstream call
