<!-- Created: 2026-05-11 by Constructor Tech -->

# Feature: Topic Segment Introspection

- [ ] `p2` - **ID**: `cpt-cf-evbk-featstatus-topic-segment-introspection`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Operator / Observability Tooling](#21-operator--observability-tooling)
  - [2.2 Replay / Backfill Tooling](#22-replay--backfill-tooling)
  - [2.3 Consumer Offset-Adviser (post-MVP, R57)](#23-consumer-offset-adviser-post-mvp-r57)
  - [2.4 Producer Health-Check](#24-producer-health-check)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [Pagination Semantics](#pagination-semantics)
  - [Backend Liveness](#backend-liveness)
  - [Authorization](#authorization)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Endpoint and Backend Integration](#endpoint-and-backend-integration)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Unit Test Plan](#7-unit-test-plan)
- [8. E2E Test Plan](#8-e2e-test-plan)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

`GET /v1/topics/segments` exposes the storage backend's segment manifest for a `(topic, partition)` pair. The response carries the offset envelope (`start_sequence`, `end_sequence`), the time envelope (`start_time`, `end_time`), and an opaque `segments[]` array whose entries are backend-specific. The endpoint surfaces information that the broker itself does not track — segment boundaries, oldest / newest event timestamps, current high-water-mark — and lets external tooling answer questions the broker's own bookkeeping cannot.

### 1.2 Purpose

Topic segment introspection gives operators, observability tooling, and external replay/backfill systems read-access to a partition's persisted contents — segment boundaries, oldest/newest event timestamps, current high-water-mark — that the broker itself does not track. The feature decouples external operational tooling from the storage backend's internal layout (Kafka segments vs. DB row spans vs. S3 object boundaries) while preserving each backend's native semantics, so dashboards, capacity-planning jobs, replay schedulers, and producer health-checks can target one stable REST surface regardless of which storage backend is in use.

Without it, operators have no way to answer routine retention / replay / lag questions short of querying each backend through its own native API — defeating the broker's pluggable-storage abstraction.

### 1.3 Actors

- **Operator / observability tooling**: dashboards, capacity planning.
- **Replay / backfill tooling**: external job that needs to know offset range before scheduling a re-process.
- **Consumer SDK (post-MVP, R57)**: offset-adviser for seek-to-earliest.
- **Producer health-check**: producer cross-checking its perceived state vs. the broker's persisted state.

### 1.4 References

- DESIGN.md §3.3 `GET /v1/topics/segments` endpoint summary
- DESIGN.md §3.2 storage-backend trait — `backend.segments(ctx, topic) -> Vec<Segment>`
- DESIGN.md §3.7 reference to `received` / `max_offset` derivability
- DESIGN.md §4.6 Out of Scope (the future subscription-based backfill that supersedes external replay tooling)
- `openapi.yaml#/paths/~1v1~1topics~1segments`

## 2. Actor Flows (CDSL)

Pseudo-code below is Python-ish — not runnable; intent over syntax.

### 2.1 Operator / Observability Tooling

```python
# Scraped every 30s per (topic, partition) by Prometheus / Grafana
def scrape_lag_metrics(topic: str, partition: int) -> dict:
    resp = http.get(f"/v1/topics/segments?topic={topic}&partition={partition}")
    env  = resp.json
    cursor_offset = cursor.get((group, topic, partition))  # from broker-side cursor cache
    return {
        "lag":              env["end_sequence"] - cursor_offset,
        "segment_count":    len(env["segments"]),
        "oldest_event_ts":  env["start_time"],
        "newest_event_ts":  env["end_time"],
    }

# AUTHZ: requires `topic:read` permission on T (same scope as GET /v1/topics).
```

### 2.2 Replay / Backfill Tooling

```python
# Operator wants to re-process events for (T, p) between [t_start, t_end]
def plan_backfill(topic, partition, t_start, t_end):
    env = http.get(f"/v1/topics/segments?topic={topic}&partition={partition}").json
    # Linear interpolation across the envelope to convert timestamps -> offsets
    off_start = interpolate_offset(env, t_start)
    off_end   = interpolate_offset(env, t_end)
    schedule_reprocess_job(topic, partition, off_start, off_end)

# In MVP there is no broker-side replay endpoint. This flow is informational
# only — the feature exists so the future subscription-based backfill (per
# DESIGN.md §4.6) has a clean place to read offset boundaries from.
```

### 2.3 Consumer Offset-Adviser (post-MVP, R57)

```python
# Consumer wants to skip past events that all filters would reject
def maybe_skip_to_earliest(topic, partition, cursor_offset):
    env = http.get(f"/v1/topics/segments?topic={topic}&partition={partition}").json
    if cursor_offset < env["start_sequence"]:
        # consumer is behind retention; SEEK forward to the earliest still-retained
        http.post(f"/v1/subscriptions/{sub_id}:seek", json={
            "partition_positions": {partition: env["start_sequence"]}
        })
    # else: keep normal polling

# Pairs with the R57 offset-adviser feature (post-MVP). Until R57 lands,
# consumers can call this endpoint manually before a seek-to-earliest.
# AUTHZ: same `topic:read` permission as 2.1.
```

### 2.4 Producer Health-Check

```python
# Producer cross-checks its chain state against the broker's persisted state
def check_publish_pipeline(producer_id, topic, partition):
    cursors  = http.get(f"/v1/producers/{producer_id}/cursors").json
    segments = http.get(f"/v1/topics/segments?topic={topic}&partition={partition}").json

    last_seq = cursors[(topic, partition)]["last_sequence"]
    end_seq  = segments["end_sequence"]

    if last_seq < end_seq - LAG_THRESHOLD:
        log.warn("publish pipeline lag", lag=end_seq - last_seq)
        # events accepted by ingest but not yet visible on the consumer side
    elif last_seq > end_seq:
        log.error("publish desync — producer ahead of backend")
        pause_publishes()
        alert_operators()
        # producer believes it published events the backend did not persist
        # (ingest outbox backlog, partial failure, etc.)

# AUTHZ: `topic:read` on T + the existing producer-cursor permission.
```

## 3. Processes / Business Logic (CDSL)

### Pagination Semantics

The endpoint paginates **segments**, not events. `$orderby` defaults to `start_sequence asc` (oldest segment first); `limit` caps the number of segment entries returned per page (default 100, max 100). The envelope fields (`start_sequence`, `end_sequence`, `start_time`, `end_time`) cover the FULL backend retention for the partition regardless of how many segments fit in the current page — they are computed against the backend's full manifest, not against the paginated subset.

### Backend Liveness

| Backend state | Endpoint response |
|---|---|
| Healthy | 200 OK with full manifest |
| Backend unreachable | 503 Service Unavailable; `Retry-After` header set |
| Backend partially scanned (e.g., S3 listing in progress) | 200 OK with envelope reflecting whatever is known; `segments[]` may be incomplete (clients should not rely on count); header `X-Manifest-Partial: true` flags the case |
| Backend reports no segments yet (empty partition) | 200 OK with `start_sequence: 0`, `end_sequence: 0`, empty `segments[]` |

### Authorization

The endpoint is gated by `topic:read` (same as `GET /v1/topics`). Rationale: segment metadata is not more sensitive than the topic existence itself; if a caller can list the topic, knowing its segment envelope leaks no further information. (Event payloads are protected by the consumer-side authz, not the segment metadata.)

## 4. States (CDSL)

The endpoint is stateless from the broker's perspective. It reflects the backend's manifest at call time and is eventually consistent — there is no broker-side caching of segment data.

## 5. Definitions of Done

### Endpoint and Backend Integration

- [ ] `p2` - **ID**: `cpt-cf-evbk-dod-topic-segment-introspection-endpoint`

- `GET /v1/topics/segments` is implemented per `openapi.yaml`.
- `backend.segments()` is wired through the storage-backend plugin trait.
- Authorization checks `topic:read` permission via the PEP.
- Pagination over the `segments[]` array works per the documented semantics.
- Backend-unavailable case returns 503 with `Retry-After`.
- Partial-scan case sets `X-Manifest-Partial: true`.
- DESIGN.md §3.3's `GET /v1/topics/segments` entry points at this feature file for use cases.

## 6. Acceptance Criteria

- **AC-1**: A topic with 0 published events returns `start_sequence: 0`, `end_sequence: 0`, empty `segments[]`.
- **AC-2**: A topic with N segments returns at most 100 segment entries per page; the envelope reflects the full N regardless of pagination.
- **AC-3**: A caller without `topic:read` on T receives 403 Forbidden.
- **AC-4**: With the backend simulated as unreachable, the endpoint returns 503 with `Retry-After`.
- **AC-5**: An operator dashboard built on this endpoint shows lag = `end_sequence - cursor.offset` for each consumer group on a partition.

## 7. Unit Test Plan

- **envelope computation**: given a manifest of N segments, assert envelope matches min/max across all segments regardless of paginated subset.
- **pagination boundary**: 250 segments + `limit=100` returns 100 entries on first page, 100 on second, 50 on third.
- **empty partition**: manifest with zero segments returns envelope `(0, 0)` and empty `segments[]`.
- **authz denial**: caller without permission gets 403 without any backend call.

## 8. E2E Test Plan

- **S1 — happy path**: publish 1000 events to (T, 0); call segments endpoint; assert end_sequence ≥ 1000 (some may still be pipeline-lagged).
- **S2 — operator dashboard**: scrape the endpoint every 30s over 5 minutes; assert end_sequence increases monotonically while events are being produced.
- **S3 — partial scan**: configure S3-style backend with simulated slow LIST; assert `X-Manifest-Partial: true` header on response while scan is in progress; assert envelope is non-decreasing across successive calls.
- **S4 — backend down**: bring backend offline; assert 503 + `Retry-After`; restore backend; assert next call returns 200.
- **S5 — authz boundary**: caller A with `topic:read` on T1 calls for T2 (no permission) — gets 403; T1 call succeeds.
