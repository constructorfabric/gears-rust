<!-- Created: 2026-05-11 by Constructor Tech -->
<!-- Updated: 2026-05-27 by Constructor Tech (merged :poll into :stream; one event per part; heartbeat 5 s default) -->
<!-- Updated: 2026-06-22 (frame protocol: topology open + non-terminal; control frame cursor carrier — progress + terminating; terminate-on-gain rebalance + re-JOIN; 429 GroupAtCapacity admission) -->

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
  - [Topology-Change Handling (Rebalance)](#topology-change-handling-rebalance)
  - [Design Rationale](#design-rationale)
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

Both endpoints carry the same frame schema (`event` / `heartbeat` / `control` / `topology`) and the same subscription-lifecycle error model (`404 SubscriptionNotFound`, `410 SubscriptionTerminated`). They differ only in how frames are framed on the wire (multipart parts vs SSE event records).

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
- **DeliveryService** (broker): emits `event` / `heartbeat` / `control` / `topology` frames; closes responses on subscription termination.

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
    # frame is JSON with top-level `kind` in {"event", "heartbeat", "control", "topology"}
    if frame.kind == "event":
        consecutive_heartbeats = 0
        process_event(frame.payload)        # one event per part
    elif frame.kind == "heartbeat":
        consecutive_heartbeats += 1
        if consecutive_heartbeats >= K:    # SDK default K = 10  ≈ 50 s of silence
            break                           # voluntary disconnect; outer loop reopens
    elif frame.kind == "control":
        offsets.commit(frame.positions)     # feed offset/last_examined into own store
        if frame.code == "terminal":
            break                           # last frame; outer loop re-JOINs (see below)
    elif frame.kind == "topology":          # loss / version bump — non-terminal
        update_assignment(frame.assigned)   # re-run offset-store / partition-lock routines
        consecutive_heartbeats = 0
# Stream closed. Recover by why it closed:
#   saw a `terminal` control frame → re-JOIN (new subscription_id) → SEEK → reopen
#   bare close (no terminating)  → reopen same subscription_id:  200 → resume;  410 → re-JOIN
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
es.addEventListener("control",  (e) => {
  const a = JSON.parse(e.data);
  offsets.commit(a.positions);                      // cursor carrier
  if (a.code === "terminal") es.close();         // then re-JOIN and reopen
});
es.addEventListener("topology",  (e) => update_assignment(JSON.parse(e.data).assigned)); // loss/version — keep reading
es.onerror = () => { /* reopen same subscription_id; 410 → re-JOIN */ };
```

The SSE endpoint serves `text/event-stream`. Same four frame kinds; the kind is carried in the SSE `event:` line and the JSON payload in the `data:` line. Reconnect resume uses `cursor.offset` (set via the `:seek` endpoint), not SSE's `Last-Event-ID`.

## 3. Processes / Business Logic (CDSL)

### Shared Frame Schema

All transports carry the same four frame kinds. Each frame is a JSON object with a top-level `kind` field. On `/v1/events:stream` each frame is one multipart part with `Content-Type: application/json`. On `/v1/events:sse` each frame is one SSE event record with the kind in the `event:` line and the JSON in the `data:` line.

```
{ "kind": "event",     "payload": { /* one EventEnvelope (id, type, topic, partition, offset, sequence, data, …) */ } }
{ "kind": "heartbeat", "at": "<iso8601>" }
{ "kind": "control",   "code": "<control_code>",   /* "reason": "<…>" on terminal */
                       "positions": [ { "topic": "...", "partition": <int>, "offset": <i64>, "last_examined": <i64> }, ... ] }
{ "kind": "topology",  "topology_version": <int>,
                       "assigned": [ { "topic": "...", "partition": <int>, "offset": <i64>, "last_examined": <i64> }, ... ] }
```

`offset` is the session cursor for the partition; `last_examined` is the highest offset the broker has scanned for this `(consumer_group, topic, partition)` regardless of whether the subscription filter matched — so a consumer computes true lag (including server-side-filtered events) without a separate call. Both are i64.

**`topology`** is the full current assignment snapshot for the subscription (the complete set, not a delta). The broker emits it:
1. once at stream open, as the confirmed baseline — guaranteed to be the first frame; and
2. mid-stream, non-terminal, when a topology change leaves the subscription streamable: it lost one or more partitions (retaining ≥1), or only `topology_version` changed.

**`control`** is the per-partition cursor carrier: the consumer feeds `positions` into its own offset store so that on reconnect it re-SEEKs from `last_examined`, skipping server-side-filtered events rather than re-scanning them. Two codes:
- `progress` — mid-stream, conditional, carrying only the **sparse** subset of partitions that need advising (chiefly the filter-saturated case where `last_examined` drifts ahead of delivered events). The broker eventually emits one for any partition whose `last_examined` drifts beyond the delivered offset by a bounded amount, so even a fully-filtered subscription learns its frontier.
- `terminal` — the terminal control frame: the **complete** final `positions` for the full assignment, emitted as the last frame, then the broker closes the stream gracefully. The `code` states the **fact** that the subscription is ending; the **reason** (`"rebalanced"`, `"lose_all"`, `"teardown"`) rides in an optional `reason` field, so consumer recovery switches on the fact, not the cause.

The `batch` frame kind does not exist — one event per `event` frame; consumers batch at the application layer (below).

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
- `cf-gears-event-broker-sdk` ships **K = 10** as the default (≈ 50 s of silence before reconnect). Tunable via `ConsumerBuilder::heartbeat_drop_threshold(K)`.
- The re-JOIN refreshes everything (new `subscription_id`, fresh assignment, fresh connection); group cursor is preserved on the broker side.

The broker does not enforce or observe this pattern — it's purely a consumer self-healing convention.

### Topology-Change Handling (Rebalance)

A topology change is handled per subscription, by what happened to that subscription's assignment. With `gained = assigned_new \ assigned_old` and `lost = assigned_old \ assigned_new`:

| Outcome for this subscription | Broker action | Consumer action |
|---|---|---|
| Lost a partition (retains ≥1) | non-terminal `topology` frame (full reduced assignment) | drop the lost partition, keep reading on the same connection |
| `topology_version` bump, set unchanged | non-terminal `topology` frame | re-run assignment-dependent routines (offset store, locks), keep reading |
| Gained a partition | `terminal` control frame (complete final positions) as last frame, then graceful close | commit positions → re-JOIN → new `subscription_id` + assignment → SEEK → open a fresh stream |
| Lost all partitions | `terminal` control frame + graceful close | same as gain (re-JOIN) |
| Gain + loss (reshuffle) | gain dominates → `terminal` control frame + close | re-JOIN |

A subscription survives a loss (it simply streams fewer partitions) but a gain terminates it: a gained partition is unseeded and SEEK is pre-stream-only, so the stream must close to re-seed, and the subscription is recreated rather than mutated in place. The group cursor is group-scoped and survives the re-JOIN, so the new subscription resumes from the preserved position. Both transports behave identically — the `terminal` control frame and `topology` frame are carried as a multipart part (`/events:stream`) or an SSE event record (`/events:sse`).

`410 SubscriptionTerminated` is returned to any request that reuses a terminated `subscription_id` — the safety net for a consumer that missed the `terminal` control frame (e.g. it crashed). A stream that closes without a `terminal` control frame is a transient drop: the consumer reopens the same `subscription_id` and resumes on `200`.

**Handoff fence.** When a partition moves from consumer A to consumer B, the gain/loss asymmetry prevents both reading it at once: A (losing) stops on its `topology` frame immediately, while B (gaining) reaches the partition only after terminate → re-JOIN → SEEK → reopen — strictly later. No broker-held barrier is needed.

**Livelock fencing.** A rebalance computes the assignment for the final membership set once and stamps `topology_version = N+1`; a forced re-JOIN settles into generation N+1 without recomputing the assignment or disrupting peers. A fixed stabilization window (default ~`PT1S`, advertised in the JOIN response) batches a burst of membership changes into one generation bump; consumers re-JOIN with backoff + jitter.

**Admission.** A subscription always holds ≥1 partition. When a group already has as many members as partitions, a further JOIN that would receive zero partitions is refused with `429` + `Retry-After` and body `code: "GroupAtCapacity"` (distinct from the rate-limit `RateLimitExceeded`); the consumer retries the JOIN later. There are no zero-partition standbys, no standby streams, and no assignment-polling channel.

### Design Rationale

- **Terminate on gain, keep streaming on loss.** A gained partition is unseeded and SEEK is pre-stream-only, so the stream must close to re-seed; recreating the subscription (rather than mutating its assignment in place) keeps `410`-on-reuse semantics crisp. A loss needs none of this — the consumer just stops reading the dropped partition. Alternatives set aside: terminating on *any* change (forces a needless reconnect on a loss); surviving a gain via in-place re-seek (muddies `410` and mutates assignment in place).
- **Control frame is the cursor carrier; no held connection.** The `terminal` control frame delivers the complete final `positions` as the last frame, then the broker closes immediately — it does not hold the connection open for a drain window. A narrow/saturating filter needs this: its `last_examined` runs far ahead of delivered offsets, so it must re-SEEK from the true frontier to avoid re-scanning server-filtered events (resolves **R57**). A reopen `topology` baseline cannot substitute, because `PositionsNotSet` forces the re-SEEK before the new stream opens. Alternative set aside: a bare close with status-code-only recovery (loses the final frontier on a narrow filter).
- **Consumer owns durable progress.** The control frame feeds the consumer's own offset store; the broker never persists it (see [ADR-0006](../ADR/0006-offset-authority.md)). The session cursor is ephemeral and auto-advances with delivery.
- **No standbys.** Surplus consumers are refused at JOIN (`429 GroupAtCapacity`) rather than admitted with empty assignments, so the protocol needs no standby stream or assignment poll.

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
Closing   → terminal control frame sent / DELETE / consumer disconnected; cleanup
Terminated → connection ended; consumer re-JOINs to enter Idle again
```

A **loss** or a bare `topology_version` change stays within `Streaming` — the broker emits a non-terminal `topology` frame and the stream continues. A **gain** or **lose-all** transitions `Streaming → Closing`: the broker emits the `terminal` control frame and closes, and the consumer re-JOINs (a fresh subscription) to re-enter `Idle`.

## 5. Definitions of Done

### Shared Transport Layer

- [ ] `p1` - **ID**: `cpt-cf-evbk-dod-consumption-transport-shared`
- Broker emits all four frame kinds (`event`, `heartbeat`, `control`, `topology`) over both transports.
- `control` carries `positions`; `progress` (sparse, mid-stream) and `terminal` (complete, terminal) codes are both emitted.
- `topology` is the full-assignment snapshot, emitted at open and on a non-terminal change (loss / version bump).
- Rebalance: a loss → non-terminal `topology` frame (keep streaming); a gain / lose-all → `terminal` control frame + graceful close → consumer re-JOINs; `410` on reuse of the terminated id.
- Admission: a JOIN that would receive zero partitions returns `429` + `Retry-After` (`GroupAtCapacity`); no zero-partition standbys.
- Heartbeat cadence configurable, 5 s default, advertised in the JOIN response; the stabilization window is advertised likewise.
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
- AC-3: SDK consumer reconnects after K consecutive heartbeats (K = 10 default in `cf-gears-event-broker-sdk`).
- AC-4: Browser consumer opens `EventSource` against `/v1/events:sse` and receives the same four frame kinds via SSE events.
- AC-5: A partition **loss** (subscription retains ≥1) emits a non-terminal `topology` frame mid-stream without closing the connection; the consumer keeps streaming its remaining partitions.
- AC-5b: A partition **gain** (or lose-all) emits a `terminal` control frame with complete final `positions` as the last frame, then the broker closes gracefully; reuse of the terminated `subscription_id` returns `410 SubscriptionTerminated`.
- AC-5c: A JOIN that would receive zero partitions (group already full) returns `429` + `Retry-After` with body `code: "GroupAtCapacity"`.
- AC-6: `Accept: application/json` against `/v1/events:stream` returns `406 Not Acceptable`.
- AC-7: `GET /v1/events:poll` (legacy path) returns `404 Not Found`.

## 7. Unit Test Plan

- **frame-emitter**: parameterize over (transport × frame kind) and assert correct framing output (multipart boundaries / SSE `event:` lines).
- **heartbeat scheduler**: simulate idle / busy timelines, assert heartbeats emit at cadence on idle and are suppressed when events are flowing.
- **multipart parser** (consumer side): assert one event per part; reject responses where a part carries an event array.

## 8. E2E Test Plan

- **E2E-1**: Publish 100 events; consume via `/v1/events:stream`; assert all 100 arrive in monotonic order across partitions.
- **E2E-2**: Open `/v1/events:stream` against an empty topic for 30 s; assert ≥ 5 `heartbeat` frames arrive (5 s cadence).
- **E2E-3**: Open `/v1/events:stream`; cause a **loss** mid-stream (another consumer joins and takes a partition); assert a non-terminal `topology` frame arrives and the connection stays open.
- **E2E-3b**: Open `/v1/events:stream`; cause a **gain** mid-stream (a peer leaves, this consumer gains its partition); assert a `terminal` control frame with final `positions` arrives as the last frame, the connection closes, and re-JOIN yields the gained partition; reuse of the old `subscription_id` returns `410`.
- **E2E-4**: Open `/v1/events:stream` with `Accept: application/json`; assert `406 Not Acceptable`.
- **E2E-5**: SDK reconnect — block `event` flow for K × heartbeat_cadence; assert the SDK consumer voluntarily reconnects and re-JOINs.
