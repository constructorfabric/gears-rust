# Event Broker — Scenario Guide

Practical companion to [DESIGN.md](../docs/DESIGN.md) and [openapi.yaml](../docs/openapi.yaml). Each scenario is a concrete HTTP exchange: a literal request, the expected response, and the side effects the broker must produce.

Organized in two parts:

1. **How To** — the integration journey and per-area reference with mechanism summaries.
2. **Guardrails** — rejections, negative cases, and error shapes.

---

## 1. How To

### Integration Journey

To publish and consume events through the broker, follow these steps. Each step links to the scenario(s) with full coverage.

#### Step 1 — Establish a consumer group

Anonymous groups are broker-minted via REST. Named groups are registered via `types_registry` at startup — no REST call needed.

- [Create anonymous group](consumer/groups/positive-1.1-create-anonymous-group.md) — `POST /v1/consumer_groups` → broker mints GTS id; distribute it to your consumer fleet out-of-band.
- [JOIN a named group](consumer/groups/positive-1.8-named-group-join.md) — no creation step; JOIN with the well-known GTS identifier directly.

#### Step 2 — Publish events

Producers submit typed events to a topic. Default is async (`202`); opt into sync persistence with `Sync-Wait`.

- [Publish a single event (async)](producer/single/positive-1.1-publish-single-async.md) — `POST /v1/events` → `202 Accepted`.
- [Publish sync (wait=persisted)](producer/single/positive-1.2-publish-sync-wait-persisted.md) — `POST /v1/events` with `Sync-Wait` header → `201 Created`.

#### Step 3 — JOIN a subscription

A consumer instance JOINs the group with topic interests and receives its partition assignment.

- [Cold JOIN, fresh group](consumer/subscriptions/positive-1.1-cold-join-fresh-group.md) — `POST /v1/subscriptions` → `201` with `assigned[]` + `topology_version`.

#### Step 4 — SEEK the starting position

Before streaming, the consumer declares where each assigned partition begins. **This step is required** — opening the stream without it returns `409 Failed Precondition`.

**Path A — Consumer has a persistent store (service DB, browser IndexedDB, filesystem)**

Read the last processed offset from your own store and SEEK with the exact integer:

- [SEEK to exact offset](consumer/positions/positive-1.3-seek-exact-offset.md) — `POST /v1/subscriptions/{id}/positions` with integer offset from own DB.
- [SEEK to timestamp](consumer/positions/positive-1.11-seek-at-timestamp.md) — use `"at:<ISO-8601>"` sentinel for date-anchored readers; response returns resolved integer for persistence.
- [Full Path A journey](consumer/flows/flow-1.3-path-a-consumer-with-db.md) — end-to-end: JOIN → SEEK from DB → stream → persist offset → reconnect resumes correctly.

**Path B — No persistent store (one-shot scripts, stateless readers)**

SEEK with a sentinel; accept reprocessing on restart:

- [SEEK to earliest](consumer/positions/positive-1.1-seek-earliest.md) — `"earliest"` → cursor = RF − 1 (= 0 on a fresh topic where RF = 1; always ≥ 0); broker emits from RF.
- [SEEK to latest](consumer/positions/positive-1.2-seek-latest.md) — `"latest"` → broker resolves to current HWM; only future events delivered.

#### Step 5 — Stream events

Open the long-lived multipart stream and consume frames as they arrive.

- [Stream multipart frames](consumer/stream/positive-1.1-stream-multipart-frames.md) — `GET /v1/events:stream` → `200 multipart/mixed`; one event per part.
- [SSE event stream](consumer/stream/positive-1.9-sse-event-stream.md) — `GET /v1/events:sse` → `200 text/event-stream`; browser-native alternative.

> **End-to-end**: [Publish → subscribe → consume](flows/flow-1.1-publish-subscribe-consume.md) composes all steps into one coupled transcript.

---

### Parallel consumer group formation

Multiple consumer instances share a group by JOINing with the same `consumer_group` identifier. The broker rebalances partitions across all active members automatically.

**Anonymous group**: one instance calls `POST /v1/consumer_groups` and distributes the returned id out-of-band (shared DB row, ConfigMap, env var). All instances then JOIN with that id.

**Named group**: no creation step. All instances JOIN with the well-known GTS identifier — no coordination needed.

- [Second JOIN triggers rebalance](consumer/subscriptions/positive-1.11-second-join-triggers-rebalance.md) — 2nd member joins; `topology` frame splits 4 partitions 2+2.
- [Third JOIN triggers rebalance](consumer/subscriptions/positive-1.12-third-join-triggers-rebalance.md) — 3rd member joins; three-way partition split.

---

### producer/ — Publish events

#### producer/single/ — `POST /v1/events` (stateless)
- [positive-1.1 — Publish single (async)](producer/single/positive-1.1-publish-single-async.md) — `POST /v1/events` → `202 Accepted`; event enqueued in outbox.
- [positive-1.2 — Publish sync (wait=persisted)](producer/single/positive-1.2-publish-sync-wait-persisted.md) — `POST /v1/events` with `Sync-Wait` → `201 Created` after backend persist.

#### producer/batch/ — `POST /v1/events:batch`
- [positive-1.1 — Publish batch](producer/batch/positive-1.1-publish-batch.md) — `POST /v1/events:batch` → `202 Accepted`; all-or-nothing per topic+partition.

#### producer/flows/ — `POST /v1/producers`, `GET /cursors`, `POST :reset`
- [positive-1.1 — Register chained producer](producer/flows/positive-1.1-register-chained-producer.md) — `POST /v1/producers { mode: chained }` → `201` with `producer_id`.
- [positive-1.2 — Register monotonic producer](producer/flows/positive-1.2-register-monotonic-producer.md) — `POST /v1/producers { mode: monotonic }` → `201`.
- [positive-1.3 — Chained-mode sequence](producer/flows/positive-1.3-chained-mode-sequence.md) — `POST /v1/events` with `Producer-Id` header and `meta.previous/sequence`; broker deduplicates.
- [positive-1.4 — Idempotency key dedup](producer/flows/positive-1.4-idempotency-key-dedup.md) — duplicate event id returns `200` with original event; no second write.
- [positive-1.6 — Cursor recovery](producer/flows/positive-1.6-cursor-recovery.md) — `GET /v1/producers/{id}/cursors` → `[{topic, partition, last_sequence}]`; feeds next SEEK after desync.
- [positive-1.7 — Chain reset](producer/flows/positive-1.7-chain-reset.md) — `POST /v1/producers/{id}:reset` → `200`; chain state cleared, audited.

---

### consumer/ — Consume events

#### consumer/groups/ — `POST/GET/DELETE /v1/consumer_groups`
- [positive-1.1 — Create anonymous group](consumer/groups/positive-1.1-create-anonymous-group.md) — `POST /v1/consumer_groups` → `201` with broker-minted GTS id.
- [positive-1.2 — Get group by id](consumer/groups/positive-1.2-get-group-by-id.md) — `GET /v1/consumer_groups/{id}` → full group record.
- [positive-1.3 — List groups](consumer/groups/positive-1.3-list-groups.md) — `GET /v1/consumer_groups` → paged list of caller-visible groups.
- [positive-1.4 — Delete empty group](consumer/groups/positive-1.4-delete-empty-group.md) — `DELETE /v1/consumer_groups/{id}` → `204`; only when no active subscriptions.
- [positive-1.8 — Named group JOIN (no create step)](consumer/groups/positive-1.8-named-group-join.md) — JOIN with `types_registry`-provisioned identifier; broker validates `:consume` grant.

#### consumer/subscriptions/ — `POST/GET/DELETE /v1/subscriptions`
- [positive-1.1 — Cold JOIN, fresh group](consumer/subscriptions/positive-1.1-cold-join-fresh-group.md) — `POST /v1/subscriptions` → `201` with `assigned[]` and `topology_version`.
- [positive-1.2 — Multi-topic interests](consumer/subscriptions/positive-1.2-join-multi-topic-interests.md) — JOIN with interests across two topics; partition assignment spans both.
- [positive-1.3 — Typed filter](consumer/subscriptions/positive-1.3-join-with-typed-filter.md) — `interest.filter { engine, expression }` applied per-member at delivery.
- [positive-1.4 — Multiple subscriptions (parallelism)](consumer/subscriptions/positive-1.4-parallelism-multiple-subscriptions.md) — second JOIN rebalances partitions 2+2.
- [positive-1.5 — Leave subscription](consumer/subscriptions/positive-1.5-leave-subscription.md) — `DELETE /v1/subscriptions/{id}` → `204`; triggers rebalance.
- [positive-1.9 — List subscriptions](consumer/subscriptions/positive-1.9-list-subscriptions.md) — `GET /v1/subscriptions` → paged list of active subscriptions.
- [positive-1.10 — Read subscription](consumer/subscriptions/positive-1.10-read-subscription.md) — `GET /v1/subscriptions/{id}` → current assignment and expiry.
- [positive-1.11 — Second JOIN triggers rebalance](consumer/subscriptions/positive-1.11-second-join-triggers-rebalance.md) — 2nd instance joins; partitions split 2+2; `topology` frame emitted.
- [positive-1.12 — Third JOIN triggers rebalance](consumer/subscriptions/positive-1.12-third-join-triggers-rebalance.md) — 3rd instance joins; three-way split; all streams updated.

#### consumer/positions/ — `POST /v1/subscriptions/{id}/positions` (SEEK)
- [positive-1.1 — SEEK earliest](consumer/positions/positive-1.1-seek-earliest.md) — `"earliest"` → cursor = RF − 1 (= 0 on a fresh topic where RF = 1; always ≥ 0); broker emits from RF onwards.
- [positive-1.2 — SEEK latest](consumer/positions/positive-1.2-seek-latest.md) — `"latest"` → cursor set to HWM; only future events delivered.
- [positive-1.3 — SEEK exact offset](consumer/positions/positive-1.3-seek-exact-offset.md) — integer last-processed offset from own DB; broker emits from `offset + 1`.
- [positive-1.4 — Mixed sentinels and integers](consumer/positions/positive-1.4-mixed-sentinels.md) — partitions may mix `"earliest"`, `"latest"`, and exact offsets in one request.
- [positive-1.8 — Forward SEEK while streaming](consumer/positions/positive-1.8-forward-seek-while-streaming.md) — advance cursor mid-stream (`MAX` rule); broker emits from new position.
- [positive-1.10 — Any value in retention range](consumer/positions/positive-1.10-seek-any-value-in-range.md) — any integer in `[RF−1, HWM]` is accepted.
- [positive-1.11 — SEEK at timestamp](consumer/positions/positive-1.11-seek-at-timestamp.md) — `"at:<ISO-8601>"` → resolves to first event at or after timestamp; response returns integer.
- [positive-1.12 — Timestamp before retention](consumer/positions/positive-1.12-seek-at-timestamp-before-retention.md) — timestamp before RF → clamps to RF.
- [positive-1.13 — Timestamp beyond HWM](consumer/positions/positive-1.13-seek-at-timestamp-beyond-hwm.md) — future timestamp → resolves to HWM (equivalent to `"latest"`).

#### consumer/stream/ — `GET /v1/events:stream`, `GET /v1/events:sse`
- [positive-1.1 — Multipart frames](consumer/stream/positive-1.1-stream-multipart-frames.md) — `GET /v1/events:stream` → `200 multipart/mixed`; `event`, `heartbeat`, `advisory`, `topology` frame kinds.
- [positive-1.2 — Heartbeat cadence](consumer/stream/positive-1.2-stream-heartbeat-cadence.md) — idle stream emits `heartbeat` every 5 s; keeps connection alive through proxies.
- [positive-1.3 — Topology frame on rebalance](consumer/stream/positive-1.3-stream-topology-frame-on-rebalance.md) — mid-stream JOIN by another member triggers `topology` frame with new `assigned[]`.
- [positive-1.9 — SSE event stream](consumer/stream/positive-1.9-sse-event-stream.md) — `GET /v1/events:sse` → `200 text/event-stream`; same frame schema as multipart.

#### consumer/flows/ — consumer-only end-to-end journeys
- [flow-1.1 — Two-consumer rebalance](consumer/flows/flow-1.1-two-consumer-rebalance.md) — full inline transcript: consumer A holds all partitions; B joins; rebalance; both stream.
- [flow-1.2 — PositionsNotSet recovery](consumer/flows/flow-1.2-positions-not-set-recovery.md) — SDK mis-SEEKs; broker returns `409`; SDK re-SEEKs and resumes.
- [flow-1.3 — Path A consumer with DB](consumer/flows/flow-1.3-path-a-consumer-with-db.md) — consumer reads own DB → SEEK exact offset → stream → persist offset → reconnect resumes from correct position.

---

### topics/ — Topic introspection

- [positive-1.1 — List topics](topics/positive-1.1-list-topics.md) — `GET /v1/topics` → paged list; `partitions` field exposes partition count.
- [positive-1.2 — List topic segments](topics/positive-1.2-list-topic-segments.md) — `GET /v1/topics/segments?topic=...&partition=...` → segment manifest with RF/HWM per segment.
- [positive-1.4 — List event types](topics/positive-1.4-list-event-types.md) — `GET /v1/event_types?topic=...` → paged list of event type registrations.

---

### flows/ — Coupled producer + consumer journeys

- [flow-1.1 — Publish → subscribe → consume](flows/flow-1.1-publish-subscribe-consume.md) — producer publishes 3 events; consumer creates group, JOINs, SEEKs, streams, processes.

---

## 2. Guardrails

### Auth & permissions

- [negative-1.1 — Missing bearer token](auth/negative-1.1-missing-bearer-token.md) — no `Authorization` header → `401 Unauthenticated` on any endpoint.
- [negative-1.2 — Invalid bearer token](auth/negative-1.2-invalid-bearer-token.md) — expired or malformed token → `401 Unauthenticated`.
- [negative-1.3 — No produce permission](auth/negative-1.3-insufficient-permission-produce.md) — `POST /v1/events` without `topic:produce` → `403 Permission Denied`.
- [negative-1.4 — No consume permission](auth/negative-1.4-insufficient-permission-consume.md) — `POST /v1/subscriptions` without `topic:consume` → `403 Permission Denied`.
- [negative-1.5 — Cross-tenant anonymous group](auth/negative-1.5-cross-tenant-anonymous-group.md) — tenant B JOINs tenant A's anonymous group → `403 Permission Denied`.
- [negative-1.6 — Unauthorized topic JOIN](consumer/subscriptions/negative-1.6-join-unauthorized-topic.md) — interest references a topic the principal cannot consume → `403`.

### Input validation

- [negative-1.3 — Schema validation failure](producer/single/negative-1.3-schema-validation-failure.md) — `event.data` fails JSON Schema → `422 Invalid Argument`.
- [negative-1.2 — Mixed-partition batch](producer/batch/negative-1.2-mixed-partition-batch.md) — batch events span different partitions → `400 Invalid Argument`.
- [negative-1.3 — Batch too large](producer/batch/negative-1.3-batch-too-large.md) — over 100 events or 1 MiB → `400 Invalid Argument`.
- [negative-1.7 — Too many interests](consumer/subscriptions/negative-1.7-join-too-many-interests.md) — more than 64 interests in one JOIN → `400 Invalid Argument`.
- [negative-1.6 — Invalid client_agent](consumer/groups/negative-1.6-invalid-client-agent.md) — non-ASCII or oversized `client_agent` → `400 Invalid Argument`.
- [guardrail-1.7 — Stream requires multipart Accept](consumer/stream/guardrail-1.7-stream-accept-json-rejected.md) — `Accept: application/json` on `:stream` endpoint → `406 Invalid Argument`.
- [guardrail-1.8 — SSE from multipart endpoint](consumer/stream/guardrail-1.8-sse-from-stream-endpoint.md) — `Accept: text/event-stream` on `/events:stream` → `406`; use `/events:sse` instead.
- [negative-1.10 — Stream rejects timeout/collect params](consumer/stream/negative-1.10-stream-rejects-timeout-collect-params.md) — unsupported query params on `:stream` → `400 Invalid Argument`.

### Seek / cursor errors

- [negative-1.5 — Out-of-range offset](consumer/positions/negative-1.5-out-of-range-offset.md) — offset below RF−1 → `400 Invalid Argument`.
- [negative-1.6 — Offset above HWM](consumer/positions/negative-1.6-offset-above-hwm.md) — offset beyond HWM → `400 Invalid Argument`.
- [negative-1.7 — Backward SEEK while streaming](consumer/positions/negative-1.7-backward-seek-while-streaming.md) — backward SEEK while `:stream` is open → `409 Failed Precondition`.
- [negative-1.9 — SEEK unassigned partition](consumer/positions/negative-1.9-seek-unassigned-partition.md) — SEEK references a partition not in `assigned[]` → `409 Failed Precondition`.

### Stream errors

- [negative-1.4 — PositionsNotSet](consumer/stream/negative-1.4-stream-positions-not-set.md) — stream opened without prior SEEK → `409 Failed Precondition`; `context.unseeded` lists affected partitions.
- [negative-1.5 — Unknown subscription](consumer/stream/negative-1.5-stream-unknown-subscription.md) — `subscription_id` not found or expired → `404 Not Found`.
- [negative-1.6 — Terminated subscription](consumer/stream/negative-1.6-stream-terminated-subscription.md) — delivery shard shutdown sends `410`; consumer re-JOINs.

### Producer chain errors

- [negative-1.5 — Chained sequence violation](producer/flows/negative-1.5-chained-sequence-violation.md) — `meta.previous` doesn't match broker's `last_sequence` → `412 Failed Precondition`; recover via `GET /v1/producers/{id}/cursors`.
- [negative-1.8 — Unknown producer](producer/flows/negative-1.8-unknown-producer.md) — `Producer-Id` not registered or reaped → `400 Invalid Argument`.

### Consumer group errors

- [negative-1.5 — Delete group with active members](consumer/groups/negative-1.5-delete-group-with-active-members.md) — `DELETE` while subscriptions exist → `409 Failed Precondition`.
- [negative-1.7 — Get unknown group](consumer/groups/negative-1.7-get-unknown-group.md) — `GET /v1/consumer_groups/{id}` for non-existent id → `404 Not Found`.
- [negative-1.8 — LEAVE unknown subscription](consumer/subscriptions/negative-1.8-leave-unknown-subscription.md) — `DELETE /v1/subscriptions/{id}` for expired/unknown id → `404 Not Found`.

### Topics / segments errors

- [negative-1.3 — Segments for unknown topic](topics/negative-1.3-segments-unknown-topic.md) — `GET /v1/topics/segments` with unregistered topic → `404 Not Found`.

### Rate limiting

- [negative-1.4 — Publish rate limited](producer/single/negative-1.4-rate-limited.md) — publish exceeds per-tenant quota → `429 Resource Exhausted` with `Retry-After`.

### Error envelope reference

- [errors-1.1 — Problem Details envelope](errors/errors-1.1-problem-details-envelope.md) — canonical RFC-9457 + GTS shape; all broker errors use this format.
- [errors-1.2 — 401 Unauthenticated](errors/errors-1.2-401-unauthenticated.md)
- [errors-1.3 — 403 Permission Denied](errors/errors-1.3-403-unauthorized.md)
- [errors-1.4 — 404 Not Found](errors/errors-1.4-404-not-found.md)
- [errors-1.5 — 409 Failed Precondition](errors/errors-1.5-409-conflict.md)
- [errors-1.6 — 412 Failed Precondition (sequence)](errors/errors-1.6-412-sequence-violation.md)
- [errors-1.7 — 429 Resource Exhausted](errors/errors-1.7-429-rate-limited.md)
- [errors-1.8 — 500 Internal](errors/errors-1.8-500-internal.md)

---

## 3. Authoring Rules

### Folder assignment

Put a scenario in the folder that matches what a reader is trying to understand — not which endpoint it calls.

| Question | Folder |
|---|---|
| "How do I publish an event?" | `producer/single/` or `producer/batch/` |
| "How does the idempotent producer protocol work?" | `producer/flows/` |
| "How do I create / manage a group?" | `consumer/groups/` |
| "How do I join / leave a subscription?" | `consumer/subscriptions/` |
| "How do I set or change my position?" | `consumer/positions/` |
| "How does the stream / SSE transport work?" | `consumer/stream/` |
| "What are the topic segment offsets?" | `topics/` |
| "What happens when auth fails?" | `auth/` |
| "What does a specific error code look like?" | `errors/` |
| "Show me a producer-only end-to-end journey" | `producer/flows/` |
| "Show me a consumer-only end-to-end journey" | `consumer/flows/` |
| "Show me publish + consume together" | `flows/` (top-level) |

### Naming convention

`{positive|negative|guardrail}-{area-number}.{seq}-{slug}.md`

Numbers are relative to the sub-area folder (restart at 1.1 for each new folder). Slugs are kebab-case; describe the behavior, not the endpoint.

### Cross-reference format

Use relative paths from the scenario file. Example from `consumer/subscriptions/`:

```markdown
[Create anonymous group](../groups/positive-1.1-create-anonymous-group.md)
```

### Flows placement

| Journey involves | Use |
|---|---|
| Only producer-side exchanges | `producer/flows/` |
| Only consumer-side exchanges | `consumer/flows/` |
| Both producer and consumer in same transcript | `flows/` (top-level) |

### Error format

All error response bodies MUST use the canonical GTS + RFC-9457 shape:

```json
{
  "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.<category>.v1~",
  "title": "<Category Label>",
  "status": <HTTP code>,
  "detail": "<human-readable detail for this occurrence>",
  "instance": "<request path>",
  "context": { "<domain fields>" }
}
```

HTTP status → category mapping:

| Status | Category | `title` |
|---|---|---|
| 400, 422 | `invalid_argument` | `"Invalid Argument"` |
| 401 | `unauthenticated` | `"Unauthenticated"` |
| 403 | `permission_denied` | `"Permission Denied"` |
| 404, 410 | `not_found` | `"Not Found"` |
| 409, 412 | `failed_precondition` | `"Failed Precondition"` |
| 429 | `resource_exhausted` | `"Resource Exhausted"` |
| 500 | `internal` | `"Internal"` |

Domain-specific fields (e.g., `unseeded`, `expected_previous`, `valid_range`) go inside `context`, not at root level.

### Side-effects predicate vocabulary

| Kind | Form |
|---|---|
| State | `<table>(<key>) is set to <value>` · `<table>(<key>) is absent` · `<table>(<key>) advances from <old> to <new>` |
| Frame | `subscription <id> next frame is <kind> with <assertion>` · `subscription <id> emits <kind> within <duration>` |
| Reply | `subsequent <call> returns <code>` |
| Lifecycle | `subscription <id> is reaped after <duration>` · `consumer-group <id> is deleted` |
| Metric / audit | `metric <name> incremented by <n>` · `audit log entry <type> created` |

### Legend

| Shorthand | Meaning |
|---|---|
| `PD` | RFC-9457 Problem Details (`application/problem+json`). Implied by any `4xx` / `5xx`. |
| `RF` | Partition retention floor — smallest offset still readable. |
| `HWM` | Partition high-water mark — offset of the next event to be admitted. |
| `MP` | Multipart frame on `:stream`. |
| `SSE` | Server-Sent Event frame on `:sse`. |
| `Cursor` | Value in `evbk_group_offsets(group, topic, partition)` — last-processed offset; broker emits from `Cursor + 1`. |
