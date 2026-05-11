<!-- Created: 2026-05-11 by Constructor Tech -->
<!-- Updated: 2026-05-27 by Constructor Tech (merged :poll into :stream; one event per part; heartbeat 5 s default) -->

# Feature: Consumption Transport

- [ ] `p1` - **ID**: `cpt-cf-evbk-featstatus-consumption-transport`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Multipart Streaming (`/events:stream`)](#21-multipart-streaming-eventsstream)
  - [2.2 Server-Sent Events (`/events:sse`)](#22-server-sent-events-eventssse)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Shared Frame Schema](#shared-frame-schema)
  - [Application-Level Batching](#application-level-batching)
  - [Heartbeat Cadence](#heartbeat-cadence)
  - [Drop-on-Nth-Heartbeat Recovery](#drop-on-nth-heartbeat-recovery)
  - [Topology-Change Handling Per Transport](#topology-change-handling-per-transport)
  - [Operator Enable / Disable](#operator-enable--disable)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Shared Transport Layer](#shared-transport-layer)
  - [Per-Transport Deliverables](#per-transport-deliverables)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Unit Test Plan](#7-unit-test-plan)
- [8. E2E Test Plan](#8-e2e-test-plan)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

The broker exposes two consumption-transport endpoints for delivering events to subscriptions:

| Transport | Endpoint | Wire format | Best for |
|---|---|---|---|
| **Multipart streaming** (default) | `GET /v1/events:stream` | `multipart/mixed; boundary=...` + `Transfer-Encoding: chunked` | Server-to-server SDKs (Rust / Go / Python / Node) — high-throughput, multi-topic / multi-partition consumers |
| **Server-Sent Events** (opt-in) | `GET /v1/events:sse` | `text/event-stream` | Browser-direct or `EventSource`-style consumers |

Both endpoints carry the same frame schema (`event` / `heartbeat` / `advisory` / `topology`) and the same subscription-lifecycle error model (`404 SubscriptionNotFound`, `410 SubscriptionTerminated`). They differ only in how frames are framed on the wire (multipart parts vs SSE event records).

Both endpoints are **delivery-only**. Cursor advance (SEEK) happens via the dedicated endpoint `POST /v1/subscriptions/{id}:seek`.

### 1.2 Purpose

Provide a single, long-lived streaming consumption transport that:
- delivers events with minimal latency (no request-response overhead, no fixed polling cycles);
- keeps idle connections alive across HTTP intermediaries via heartbeat frames;
- pushes batching responsibility to the consumer (application-level), giving it full control over commit / ack boundaries;
- offers a browser-friendly variant (SSE) without fragmenting the protocol design.

### 1.3 Actors

- **Consumer SDK / process**: opens the stream, reads frames, dispatches events to handlers, seeks (advances cursor) via the `:seek` endpoint.
- **Browser app (optional)**: opens an `EventSource` against `/events:sse`.
- **DeliveryService** (broker): emits `event` / `heartbeat` / `advisory` / `topology` frames; closes responses on subscription termination.

### 1.4 References

- [RFC 2046](https://www.rfc-editor.org/rfc/rfc2046) — `multipart/*` media types (`multipart/mixed` is the chosen subtype)
- [RFC 7230 §3.3.1](https://www.rfc-editor.org/rfc/rfc7230#section-3.3.1) — HTTP `Transfer-Encoding: chunked`
- [HTML Living Standard — Server-Sent Events](https://html.spec.whatwg.org/multipage/server-sent-events.html)
- [features/0002-consumer-subscription-lifecycle.md](0002-consumer-subscription-lifecycle.md) — JOIN / re-JOIN / LEAVE
- [DESIGN.md §3.3](../DESIGN.md) — API Contracts
- [openapi.yaml](../openapi.yaml) — authoritative REST surface

## 2. Actor Flows (CDSL)

### 2.1 Multipart Streaming (`/events:stream`)

```python
# Consumer side: open a long-lived multipart connection, iterate frames as they arrive.
resp = http.get(
    f"/v1/events:stream?subscription_id={sub_id}",
    headers={"Accept": "multipart/mixed"},  # also accepted: */* (defaults to multipart/mixed)
    stream=True,
)

consecutive_heartbeats = 0
for frame in parse_multipart(resp):   # iterator over multipart parts as they arrive
    # frame is JSON with top-level `kind` in {"event", "heartbeat", "advisory", "topology"}
    if frame.kind == "event":
        consecutive_heartbeats = 0
        process_event(frame.payload)        # one event per part
    elif frame.kind == "heartbeat":
        consecutive_heartbeats += 1
        if consecutive_heartbeats >= K:    # SDK default K = 10  ≈ 50 s of silence
            break                           # voluntary disconnect; outer loop re-JOINs
    elif frame.kind == "advisory":
        log_advisory(frame.payload)
    elif frame.kind == "topology":
        update_assignment(frame.payload)
        consecutive_heartbeats = 0
# Closing the response is graceful. Caller decides whether to reconnect, re-JOIN, or stop.
```

Notes:
- The connection is long-lived. The server emits frames continuously; the client reads as fast as it can.
- Each multipart part carries **exactly one** event (no server-side batching).
- Heartbeats arrive at the broker's configured cadence (default 5 s) on idle subscriptions; busy subscriptions suppress them.
- Cursor advance (SEEK) is out-of-band via `POST /v1/subscriptions/{id}:seek`. The stream connection plays no role in cursor management.
- **Pre-stream SEEK is required.** The SDK MUST call `POST /v1/subscriptions/{id}:seek` after JOIN (with positions resolved via `OffsetManager::position(...)`) before opening the stream. Opening `:stream` without seeded cursors returns `409 PositionsNotSet { unseeded: [(topic, partition), ...], recovery_hint }` — a defensive backstop the well-behaved SDK never observes on the happy path. See `features/0002-consumer-subscription-lifecycle.md` §2.1 for the full flow and `DESIGN.md` §3.3 for the start-position resolution semantics.

### 2.2 Server-Sent Events (`/events:sse`)

```javascript
// Browser side.
const es = new EventSource(`/v1/events:sse?subscription_id=${subId}`);

es.addEventListener("event",     (e) => process_event(JSON.parse(e.data)));
es.addEventListener("heartbeat", (e) => /* optional: drop-on-Nth-heartbeat */);
es.addEventListener("advisory",  (e) => log_advisory(JSON.parse(e.data)));
es.addEventListener("topology",  (e) => update_assignment(JSON.parse(e.data)));
es.onerror = () => { /* EventSource handles reconnect; caller may re-JOIN if subscription is gone */ };
```

The SSE endpoint serves `text/event-stream`. Same four frame kinds; the kind is carried in the SSE `event:` line and the JSON payload in the `data:` line. Reconnect resume uses `cursor.offset` (set via the `:seek` endpoint), not SSE's `Last-Event-ID`.

## 3. Processes / Business Logic (CDSL)

### Shared Frame Schema

All transports carry the same four frame kinds. Each frame is a JSON object with a top-level `kind` field. On `/v1/events:stream` each frame is one multipart part with `Content-Type: application/json`. On `/v1/events:sse` each frame is one SSE event record with the kind in the `event:` line and the JSON in the `data:` line.

```
{ "kind": "event",     "payload": { /* one EventEnvelope (id, type, topic, partition, offset, sequence, data, …) */ } }
{ "kind": "heartbeat", "at": "<iso8601>" }
{ "kind": "advisory",  "code": "<advisory_code>", "detail": "<human readable>" }
{ "kind": "topology",  "version": <int>, "assigned": [ { "topic": "...", "partition": <int>, "offset": <i64>, "last_examined": <i64> }, ... ] }
```

The previous `batch` frame kind is **retired** — it always carried one event in v1 anyway after the server-side `collect` window was removed, and `event` is the accurate name.

### Application-Level Batching

The broker emits one event per frame. Consumers that want to batch (commit N events in one DB transaction, send N events in one downstream HTTP call, etc.) batch at the application layer — typically anchored to a commit boundary that matters to the consumer (DB txn, ack horizon, time-bounded flush). This gives the consumer full control over batching semantics without coupling broker behavior to consumer commit shape.

### Heartbeat Cadence

- **Default**: 5 seconds.
- **Configurable** per deployment via broker configuration (operator concern).
- **Exposed** to the consumer in the JOIN response (`heartbeat_interval_ms`) so SDKs can scale their drop-on-Nth-heartbeat threshold proportionally.
- **Suppressed when busy**: an `event` frame resets the heartbeat-idle timer. Heartbeats only emit when the broker has had no events for the subscription within the cadence interval.

The 5 s default comfortably undercuts common HTTP intermediary idle-cut thresholds (corp proxies ~60 s, AWS NLB 350 s default, ALB 60 s default).

### Drop-on-Nth-Heartbeat Recovery

Heartbeats prove the broker is alive but say nothing about whether the consumer's view of the subscription is fresh. If the connection has been silently degraded (mid-path NAT churn, ALB rebalance, etc.), the consumer can use a defensive recovery pattern:

- After **K consecutive `heartbeat` frames** with no intervening `event` frame, the consumer voluntarily disconnects from the stream and re-JOINs the subscription.
- `cyberware-event-broker-sdk` ships **K = 10** as the default (≈ 50 s of silence before reconnect). Tunable via `ConsumerBuilder::heartbeat_drop_threshold(K)`.
- The re-JOIN refreshes everything (new `subscription_id`, fresh assignment, fresh connection); group cursor is preserved on the broker side.

The broker does not enforce or observe this pattern — it's purely a consumer self-healing convention.

### Topology-Change Handling Per Transport

| Transport | Topology change |
|---|---|
| `/events:stream` | Broker emits a `topology` frame mid-stream; consumer updates its assignment cache and keeps reading on the same connection. Existing in-flight `event` frames continue to flow. |
| `/events:sse` | Same — broker emits a `topology` SSE event; browser handler updates UI / state accordingly. |

### Operator Enable / Disable

Each transport can be enabled / disabled per deployment:

| Transport | Default | Notes |
|---|---|---|
| `/events:stream` | enabled | Required v1 baseline. Disabling it breaks all server-to-server consumers. |
| `/events:sse` | disabled in v1 | Opt-in via deployment configuration. Browser-direct consumers are not the primary v1 target. |

## 4. States (CDSL)

A single per-consumer state machine governs both transports:

```
Idle      → connecting via GET /v1/events:stream (or :sse)
Streaming → emitting frames (events + heartbeats); the steady state
Closing   → received SubscriptionTerminated / consumer disconnected; cleanup
Terminated → connection ended; consumer may re-JOIN to enter Idle again
```

Topology-change events do not transition states — they emit a `topology` frame within the `Streaming` state.

## 5. Definitions of Done

### Shared Transport Layer

- Broker emits all four frame kinds (`event`, `heartbeat`, `advisory`, `topology`) over both transports.
- Heartbeat cadence configurable, 5 s default, advertised in the JOIN response.
- Subscription lifecycle (404 / 410) is surfaced identically across transports.
- One event per multipart part (no server-side batching).

### Per-Transport Deliverables

- **`/v1/events:stream`**:
  - `multipart/mixed` over chunked transfer encoding
  - Long-lived response
  - `Accept` header negotiation: `multipart/mixed` or `*/*` → served; anything else → `406 Not Acceptable`
  - Heartbeats at 5 s cadence on idle
- **`/v1/events:sse`**:
  - `text/event-stream`
  - Same frame kinds via SSE `event:` lines
  - Opt-in via deployment configuration

## 6. Acceptance Criteria

- AC-1: Consumer reads N events from `/v1/events:stream` and receives exactly N `event` frames (one per multipart part) in offset-monotonic order per `(topic, partition)`.
- AC-2: Idle subscription emits `heartbeat` frames at the configured cadence (default 5 s).
- AC-3: SDK consumer reconnects after K consecutive heartbeats (K = 10 default in `cyberware-event-broker-sdk`).
- AC-4: Browser consumer opens `EventSource` against `/v1/events:sse` and receives the same four frame kinds via SSE events.
- AC-5: Topology change emits a `topology` frame mid-stream without closing the connection.
- AC-6: `Accept: application/json` against `/v1/events:stream` returns `406 Not Acceptable`.
- AC-7: `GET /v1/events:poll` (legacy path) returns `404 Not Found`.

## 7. Unit Test Plan

- **frame-emitter**: parameterize over (transport × frame kind) and assert correct framing output (multipart boundaries / SSE `event:` lines).
- **heartbeat scheduler**: simulate idle / busy timelines, assert heartbeats emit at cadence on idle and are suppressed when events are flowing.
- **multipart parser** (consumer side): assert one event per part; reject responses where a part carries an event array.

## 8. E2E Test Plan

- **E2E-1**: Publish 100 events; consume via `/v1/events:stream`; assert all 100 arrive in monotonic order across partitions.
- **E2E-2**: Open `/v1/events:stream` against an empty topic for 30 s; assert ≥ 5 `heartbeat` frames arrive (5 s cadence).
- **E2E-3**: Open `/v1/events:stream`; cause a topology change mid-stream; assert a `topology` frame arrives without connection close.
- **E2E-4**: Open `/v1/events:stream` with `Accept: application/json`; assert `406 Not Acceptable`.
- **E2E-5**: SDK reconnect — block `event` flow for K × heartbeat_cadence; assert the SDK consumer voluntarily reconnects and re-JOINs.
