---
status: proposed
date: 2026-02-03
decision-makers: OAGW Team
---

# Backpressure and Queueing — Reject and Queue Strategies for Overload Handling


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Strategy 1: Reject (Default)](#strategy-1-reject-default)
  - [Strategy 2: Queue](#strategy-2-queue)
  - [Client Signaling](#client-signaling)
  - [Interaction with Circuit Breaker](#interaction-with-circuit-breaker)
  - [Deferred: Degrade Strategy](#deferred-degrade-strategy)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Reject strategy](#reject-strategy)
  - [Queue strategy](#queue-strategy)
  - [Degrade strategy](#degrade-strategy)
- [More Information](#more-information)
  - [Reject Response Example](#reject-response-example)
  - [Queue Flow](#queue-flow)
  - [Timeout Handling Response](#timeout-handling-response)
  - [Request Priority (Future Enhancement)](#request-priority-future-enhancement)
  - [Memory Management](#memory-management)
  - [Exponential Backoff Recommendation](#exponential-backoff-recommendation)
  - [Integration with Concurrency Control](#integration-with-concurrency-control)
  - [Metrics](#metrics)
  - [Error Types](#error-types)
  - [Database Schema](#database-schema)
  - [Defaults](#defaults)
  - [Security Considerations](#security-considerations)
  - [Implementation Phases](#implementation-phases)
  - [Configuration Validation](#configuration-validation)
  - [Testing Strategy](#testing-strategy)
  - [References](#references)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-backpressure-queueing`

## Context and Problem Statement

When concurrency or rate limits are exceeded, OAGW needs a strategy beyond simple rejection. Immediately rejecting requests causes poor user experience during traffic spikes, thundering herd effects (many clients retry simultaneously), wasted work (client may have already sent request body), and cascading failures. OAGW needs backpressure mechanisms to gracefully handle overload and signal clients to slow down.

## Decision Drivers

* Smooth degradation during traffic spikes (avoid hard rejections)
* Prevent resource exhaustion (bounded memory/queue size)
* Signal clients to back off (HTTP 503 + Retry-After)
* Work with both rate limiting and concurrency limiting
* Observable queue behavior (depth, wait time, rejections)
* Configurable strategies per upstream/route

## Considered Options

* Reject strategy (immediate error, default)
* Queue strategy (enqueue with timeout)
* Degrade strategy (fallback routing or cached response)

## Decision Outcome

Chosen option: "Implement `reject` and `queue` strategies (Phase 1-2)", because they provide graceful degradation during traffic spikes with bounded resource usage and clear client signaling.

### Strategy 1: Reject (Default)

Immediately return error to client. Fast-fail for APIs where retry logic is client's responsibility. Response: `503 Service Unavailable` with `Retry-After` header.

### Strategy 2: Queue

Enqueue request until capacity available or timeout. Smooths traffic bursts without rejecting requests. Configuration:

```json
{
  "concurrency_limit": {
    "max_concurrent": 100,
    "strategy": "queue",
    "queue": {
      "max_depth": 500,
      "timeout": "5s",
      "ordering": "fifo",
      "memory_limit": "10MB",
      "overflow_strategy": "drop_newest"
    }
  }
}
```

Queue overflow: `drop_newest` (default, preserves FIFO fairness), `reject` (immediate 503), or `drop_oldest` (evict oldest queued request).

### Client Signaling

All backpressure responses include `Retry-After` header. Calculation: concurrency-limited uses estimated wait time (avg request duration), rate-limited uses window reset time, default 1s. Clients recommended to use exponential backoff with ±20% jitter.

### Interaction with Circuit Breaker

When OPEN: queue does not accumulate requests (immediate rejection). When HALF-OPEN: queue operates normally, allows probe requests.

### Deferred: Degrade Strategy

Route to lower-priority endpoint pool, return cached response, or forward to fallback upstream. Deferred to Phase 3-4 until demonstrated need.

### Consequences

* Good, because smoother traffic handling during bursts
* Good, because better UX (fewer immediate rejections)
* Good, because automatic retry handling via queueing
* Good, because observable queue behavior (metrics for depth, wait time, memory)
* Bad, because memory overhead for queues
* Bad, because increased latency (queueing delay)
* Bad, because complexity in queue management
* Bad, because risk of timeout cascades (client timeout < queue timeout)

### Confirmation

Unit tests verify: queue enqueue/dequeue correctness, timeout expiration, memory limit enforcement, FIFO ordering. Integration tests verify: queueing under concurrency limit, timeout cascade scenarios, queue overflow behavior, permit release triggers queue consumer.

## Pros and Cons of the Options

### Reject strategy

* Good, because no memory overhead, predictable latency, simple
* Bad, because poor UX during spikes, clients must implement retry logic

### Queue strategy

* Good, because absorbs traffic spikes, better UX, automatic retry handling
* Bad, because memory overhead, increased latency, timeout cascade risk

### Degrade strategy

* Good, because maintains availability, graceful degradation
* Bad, because complex configuration, requires fallback infrastructure

## More Information

### Reject Response Example

```http
HTTP/1.1 503 Service Unavailable
Retry-After: 1
X-OAGW-Error-Source: gateway

{
  "type": "gts.x.core.errors.err.v1~x.oagw.concurrency_limit.exceeded.v1",
  "title": "Concurrency Limit Exceeded",
  "status": 503,
  "detail": "Upstream api.openai.com at max concurrent requests (100/100)",
  "retry_after_seconds": 1
}
```

### Queue Flow

```text
Request arrives
  ↓
[Try acquire permit]
  ↓
  ├─ Success → Execute immediately
  └─ Failure → [Enqueue with timeout]
               ↓
               ├─ Permit available before timeout → Execute
               └─ Timeout expires → Return 503
```

### Timeout Handling Response

When queued request times out before permit available:

```http
HTTP/1.1 503 Service Unavailable
Retry-After: 2
X-OAGW-Error-Source: gateway

{
  "type": "gts.x.core.errors.err.v1~x.oagw.queue.timeout.v1",
  "title": "Queue Timeout",
  "status": 503,
  "detail": "Request queued for 5s, no capacity available",
  "queue_wait_seconds": 5.2,
  "retry_after_seconds": 2
}
```

### Request Priority (Future Enhancement)

Allow clients to specify request priority via `X-OAGW-Priority` header (0–100, default 50). Higher priority requests dequeued first.

```json
{
  "queue": {
    "ordering": "priority",
    "priority": {
      "allow_client_override": false,
      "default_priority": 50,
      "max_priority": 100
    }
  }
}
```

Security: `allow_client_override: false` prevents priority abuse. Use authentication context to assign priority.

### Memory Management

Queue tracks estimated memory per request:

```text
memory_estimate =
    headers_size +
    body_size (if buffered) +
    metadata_overhead (∼200 bytes)
```

Large request handling:

* Streaming bodies: Not buffered, only metadata queued (∼200 bytes)
* Buffered bodies: Entire request counted against `memory_limit`
* Single request too large (body exceeds `memory_limit`): Reject with `413 Payload Too Large` before enqueuing
* Aggregate queue memory exhausted (sum of all queued requests ≥ `memory_limit`): Reject with `503 Service Unavailable` (see `x.oagw.queue.memory_limit.v1`)

Queue memory tracking:

```rust
struct RequestQueue {
    items: VecDeque<QueuedRequest>,
    total_memory: AtomicUsize,
    config: QueueConfig,
}

impl RequestQueue {
    fn try_enqueue(&mut self, req: QueuedRequest) -> Result<(), QueueError> {
        let new_total = self.total_memory.load(Ordering::Relaxed) + req.estimated_size;

        if new_total > self.config.memory_limit {
            return Err(QueueError::MemoryLimitExceeded);
        }

        if self.items.len() >= self.config.max_depth {
            return Err(QueueError::QueueFull);
        }

        let size = req.estimated_size;
        self.items.push_back(req);
        self.total_memory.fetch_add(size, Ordering::Relaxed);
        Ok(())
    }
}
```

### Exponential Backoff Recommendation

```text
backoff_seconds = min(
    initial_backoff * (2 ^ retry_count),
    max_backoff
)

// Example: 1s, 2s, 4s, 8s, 16s, 30s (max)
```

Jitter: Add ±20% randomization to prevent thundering herd.

### Integration with Concurrency Control

Permit acquisition flow:

```rust
async fn handle_request(req: Request) -> Result<Response, ProxyError> {
    // 1. Check rate limit
    rate_limiter.check().await?;

    // 2. Try acquire concurrency permit
    let permit: Permit = match concurrency_limiter.try_acquire() {
        Ok(permit) => permit,
        Err(_) if config.strategy == "reject" => {
            return Err(ConcurrencyLimitExceeded);
        }
        Err(_) if config.strategy == "queue" => {
            // Enqueue: tracks req metadata for memory/timeout,
            // waits until a Permit is available or timeout expires
            queue.enqueue(&req).await?  // returns Permit
        }
        Err(_) => return Err(ConcurrencyLimitExceeded),
    };

    // 3. Execute request (permit held in scope)
    let response = upstream_client.send(req).await?;

    // 4. Permit auto-released via Drop
    Ok(response)
}
```

Queue consumer — background task continuously consumes queue when permits available:

```rust
async fn queue_consumer(queue: Arc<RequestQueue>, limiter: Arc<ConcurrencyLimiter>) {
    loop {
        // Wait for permit
        let permit = limiter.acquire().await;

        // Dequeue next request
        let req = match queue.dequeue().await {
            Some(req) if !req.is_expired() => req,
            Some(req) => {
                req.respond(QueueTimeout);
                continue;
            }
            None => {
                drop(permit);
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        };

        // Execute request
        tokio::spawn(async move {
            let (data, responder) = req.into_parts();
            let response = execute_request(data).await;
            responder.respond(response);
            drop(permit); // permit released after request completes
        });
    }
}
```

### Metrics

Queue metrics:

```promql
# Queue depth (gauge)
oagw_queue_depth{host, level} gauge
# level: "upstream", "route", "tenant"

# Queue wait time (histogram)
oagw_queue_wait_duration_seconds{host} histogram

# Queue rejections (counter)
oagw_queue_rejected_total{host, reason} counter
# reason: "queue_full", "timeout", "memory_limit"

# Queue timeouts (counter)
oagw_queue_timeout_total{host} counter

# Queue memory usage (gauge)
oagw_queue_memory_bytes{host} gauge
```

Backpressure metrics:

```promql
# Backpressure responses (counter)
oagw_backpressure_total{host, strategy, reason} counter
# strategy: "reject", "queue", "degrade"
# reason: "concurrency_limit", "rate_limit"

# Retry-After values (histogram)
oagw_retry_after_seconds{host} histogram
```

### Error Types

```json
{
  "type": "gts.x.core.errors.err.v1~x.oagw.queue.timeout.v1",
  "title": "Queue Timeout",
  "status": 503,
  "detail": "Request queued for 5s, no capacity available",
  "queue_wait_seconds": 5.2,
  "retry_after_seconds": 2
}
```

```json
{
  "type": "gts.x.core.errors.err.v1~x.oagw.queue.full.v1",
  "title": "Queue Full",
  "status": 503,
  "detail": "Request queue full (500/500), try again later",
  "queue_depth": 500,
  "max_depth": 500,
  "retry_after_seconds": 2
}
```

```json
{
  "type": "gts.x.core.errors.err.v1~x.oagw.request.payload_too_large.v1",
  "title": "Payload Too Large",
  "status": 413,
  "detail": "Request body (12MB) exceeds queue memory_limit (10MB); not enqueued",
  "request_body_bytes": 12582912,
  "memory_limit_bytes": 10485760
}
```

```json
{
  "type": "gts.x.core.errors.err.v1~x.oagw.queue.memory_limit.v1",
  "title": "Queue Memory Limit Exceeded",
  "status": 503,
  "detail": "Aggregate queue memory reached (10MB/10MB); cannot enqueue",
  "queue_memory_bytes": 10485760,
  "memory_limit_bytes": 10485760,
  "retry_after_seconds": 1
}
```

### Database Schema

Queue configuration stored in upstream/route `concurrency_limit` or `rate_limit` fields. No additional database tables needed (in-memory queue only).

### Defaults

If not specified:

```json
{
  "strategy": "reject",
  "queue": {
    "max_depth": 100,
    "timeout": "5s",
    "ordering": "fifo",
    "memory_limit": "100MB",
    "overflow_strategy": "drop_newest"
  }
}
```

### Security Considerations

**Queue Exhaustion Attack**: Malicious client floods OAGW to fill queues. Mitigations: per-tenant rate limiting (limits requests before queueing), memory limits (prevents unbounded queue growth), authentication required (prevents anonymous flooding), monitor `oagw_queue_depth` for anomalies.

**Priority Abuse**: Client claims high priority for all requests. Mitigations: `allow_client_override: false` by default, priority assigned by authentication context (tenant tier), audit high-priority requests.

### Implementation Phases

**Phase 1: Basic Queueing** — `reject` and `queue` strategies, FIFO ordering, timeout handling, max depth enforcement, metrics.

**Phase 2: Memory Management** — Memory tracking and limits, large request handling, overflow strategies.

**Phase 3: Priority Queueing** (Future) — Priority-based ordering, client priority override (optional), priority fairness algorithms.

**Phase 4: Degradation** (Future) — `degrade` strategy, fallback upstream routing, cached response fallback.

### Configuration Validation

Configuration validation rules: `max_depth` 1–10,000; `timeout` 1–60s; `memory_limit` 1B–1GB; `strategy: "queue"` requires `queue` config; `ordering: "priority"` requires `priority` config.

### Testing Strategy

**Unit Tests**: Queue enqueue/dequeue correctness, timeout expiration handling, memory limit enforcement, FIFO ordering.

**Integration Tests**: Queueing under concurrency limit, timeout cascade scenarios, queue overflow behavior, permit release triggers queue consumer.

**Load Tests**: Sustain queue at max_depth, verify no memory leaks, measure queueing latency overhead, concurrent enqueue/dequeue.

### References

- [ADR: Concurrency Control](./0011-concurrency-control.md) — In-flight limits
- [ADR: Rate Limiting](./0004-rate-limiting.md) — Time-based rate control
- [ADR: Circuit Breaker](./0005-circuit-breaker.md) — Upstream health protection
- [Envoy Circuit Breaking](https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/upstream/circuit_breaking)
- [AWS Lambda Throttling](https://docs.aws.amazon.com/lambda/latest/dg/invocation-async.html#invocation-async-throttling)
- [Google Cloud Tasks Retry](https://cloud.google.com/tasks/docs/creating-http-target-tasks#retry)

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-fr-rate-limiting` — Queue strategy as alternative to reject for rate limit exceeded
* `cpt-cf-oagw-usecase-rate-limit-exceeded` — Strategy-based handling (reject/queue/degrade)
* `cpt-cf-oagw-nfr-high-availability` — Graceful degradation during traffic spikes
