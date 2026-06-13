# Event Broker — API Scenario Guide

Practical companion to [DESIGN.md](../docs/DESIGN.md) and [openapi.yaml](../docs/openapi.yaml). Each scenario is a concrete HTTP exchange against the broker's REST/streaming API: a literal request, the expected response, and the side effects the broker must produce. The SDK is one client of this API; scenarios describe the contract any client (SDK, curl, future gRPC bridge) must satisfy.

Organized in three parts:

1. **Integration Journey** — the happy-path walkthrough an integrator follows to go from "nothing" to "consuming events", with links to per-step scenarios.
2. **Reference by Area** — flat table of contents, every scenario linked.
3. **Rules & Legend** — authoring rules every scenario satisfies, plus the assertion shorthand vocabulary.

---

## 1. Integration Journey

To publish and consume events through the broker, follow these steps. Each links to the scenario(s) with full coverage.

#### Step 1 — Establish a consumer group

Anonymous groups are minted by the broker; named groups are registered via `types_registry` at startup.

- [Create an anonymous group](consumer-groups/positive-1.1-create-anonymous-group.md) — `POST /v1/consumer_groups`, broker mints the GTS id.

#### Step 2 — Publish events

Producers submit typed events to a topic. Default is async (`202`); opt into synchronous persistence with `Sync-Wait`.

- [Publish a single event (async)](events/positive-4.1-publish-single-async.md) — `POST /v1/events` → `202 Accepted`.

#### Step 3 — JOIN a subscription

A consumer instance JOINs the group with one or more topic-anchored interests and receives its partition assignment.

- [Cold JOIN against a fresh group](subscriptions/positive-2.1-cold-join-fresh-group.md) — `POST /v1/subscriptions` → `201` with `assigned[]` + `topology_version`.

#### Step 4 — SEEK the starting position

Before streaming, the consumer declares where each assigned partition begins — an explicit last-processed offset, or the `"earliest"` / `"latest"` sentinel (server-resolved at admission). **This step is required** — opening the stream without it returns `409 PositionsNotSet`.

- [Pre-stream SEEK to earliest](positions/positive-3.1-pre-stream-seek-earliest.md) — `POST /v1/subscriptions/{id}/positions` with `"earliest"`.

#### Step 5 — Stream events

Open the long-lived multipart stream and consume frames as they arrive (one event per part).

- [Stream multipart frames](transports/positive-6.1-stream-multipart-frames.md) — `GET /v1/events:stream` → `200 multipart/mixed`.

#### Step 6 — Acknowledge progress

Advance the cursor past processed events via the positions endpoint (forward-only while streaming).

- [Mid-stream forward ack](positions/positive-3.8-mid-stream-forward-ack.md) — `POST /v1/subscriptions/{id}/positions` with an advancing integer offset.

> **End-to-end**: [Publish → subscribe → consume → ack](flows/flow-7.1-publish-subscribe-consume-ack.md) composes all six steps into one journey.

---

## 2. Reference by Area

### consumer-groups/ — `POST/GET/DELETE /v1/consumer_groups`
- [positive-1.1 — Create anonymous group](consumer-groups/positive-1.1-create-anonymous-group.md)
- [positive-1.2 — Get group by id](consumer-groups/positive-1.2-get-group-by-id.md)
- [positive-1.3 — List groups](consumer-groups/positive-1.3-list-groups.md)
- [positive-1.4 — Delete empty group](consumer-groups/positive-1.4-delete-empty-group.md)
- [negative-1.5 — Delete group with active members](consumer-groups/negative-1.5-delete-group-with-active-members.md)
- [negative-1.6 — Invalid (non-ASCII) field](consumer-groups/negative-1.6-invalid-client-agent.md)
- [negative-1.7 — Get unknown group](consumer-groups/negative-1.7-get-unknown-group.md)

### subscriptions/ — `POST /v1/subscriptions`, `DELETE /v1/subscriptions/{id}`
- [positive-2.1 — Cold JOIN against a fresh group](subscriptions/positive-2.1-cold-join-fresh-group.md)
- [positive-2.2 — JOIN with multi-topic interests](subscriptions/positive-2.2-join-multi-topic-interests.md)
- [positive-2.3 — JOIN with typed filter](subscriptions/positive-2.3-join-with-typed-filter.md)
- [positive-2.4 — parallelism: multiple subscriptions](subscriptions/positive-2.4-parallelism-multiple-subscriptions.md)
- [positive-2.5 — LEAVE subscription](subscriptions/positive-2.5-leave-subscription.md)
- [negative-2.6 — JOIN unauthorized topic](subscriptions/negative-2.6-join-unauthorized-topic.md)
- [negative-2.7 — JOIN too many interests](subscriptions/negative-2.7-join-too-many-interests.md)
- [negative-2.8 — LEAVE unknown subscription](subscriptions/negative-2.8-leave-unknown-subscription.md)

### positions/ — `POST /v1/subscriptions/{id}/positions` (SEEK + forward ack)
- [positive-3.1 — Pre-stream SEEK to earliest](positions/positive-3.1-pre-stream-seek-earliest.md)
- [positive-3.2 — Pre-stream SEEK to latest](positions/positive-3.2-pre-stream-seek-latest.md)
- [positive-3.3 — Pre-stream SEEK to exact offset](positions/positive-3.3-pre-stream-seek-exact-offset.md)
- [positive-3.4 — Mixed int and sentinels](positions/positive-3.4-mixed-int-and-sentinels.md)
- [negative-3.5 — Out-of-range offset](positions/negative-3.5-out-of-range-offset.md)
- [negative-3.6 — Offset above HWM](positions/negative-3.6-offset-above-hwm.md)
- [negative-3.7 — Mid-stream backward seek](positions/negative-3.7-mid-stream-backward-seek.md)
- [positive-3.8 — Mid-stream forward ack](positions/positive-3.8-mid-stream-forward-ack.md)
- [negative-3.9 — Seek unassigned partition](positions/negative-3.9-seek-unassigned-partition.md)
- [positive-3.10 — Pre-stream any value in range](positions/positive-3.10-pre-stream-any-value-in-range.md)

### events/ — `POST /v1/events`, `POST /v1/events:batch`
- [positive-4.1 — Publish single (async)](events/positive-4.1-publish-single-async.md)
- [positive-4.2 — Publish sync (wait=persisted)](events/positive-4.2-publish-sync-wait-persisted.md)
- [positive-4.3 — Publish batch](events/positive-4.3-publish-batch.md)
- [positive-4.4 — Chained-mode sequence](events/positive-4.4-chained-mode-sequence.md)
- [positive-4.5 — Idempotency dedup](events/positive-4.5-idempotency-key-dedup.md)
- [negative-4.6 — Chained sequence violation](events/negative-4.6-chained-sequence-violation.md)
- [negative-4.7 — Schema validation failure](events/negative-4.7-schema-validation-failure.md)
- [negative-4.8 — Mixed-topic batch](events/negative-4.8-mixed-partition-batch.md)
- [negative-4.9 — Batch too large](events/negative-4.9-batch-too-large.md)
- [negative-4.10 — Rate limited](events/negative-4.10-rate-limited.md)

### topics/ — `GET /v1/topics`, `GET /v1/topics/segments`
- [positive-5.1 — List topics](topics/positive-5.1-list-topics.md)
- [positive-5.2 — List topic segments](topics/positive-5.2-list-topic-segments.md)
- [negative-5.3 — Segments for unknown topic](topics/negative-5.3-segments-unknown-topic.md)

### transports/ — `GET /v1/events:stream` (multipart), `GET /v1/events:sse`
- [positive-6.1 — Stream multipart frames](transports/positive-6.1-stream-multipart-frames.md)
- [positive-6.2 — Stream heartbeat cadence](transports/positive-6.2-stream-heartbeat-cadence.md)
- [positive-6.3 — Stream topology frame on rebalance](transports/positive-6.3-stream-topology-frame-on-rebalance.md)
- [negative-6.4 — Stream PositionsNotSet](transports/negative-6.4-stream-positions-not-set.md)
- [negative-6.5 — Stream unknown subscription](transports/negative-6.5-stream-unknown-subscription.md)
- [negative-6.6 — Stream terminated subscription](transports/negative-6.6-stream-terminated-subscription.md)
- [guardrail-6.7 — Stream Accept json rejected](transports/guardrail-6.7-stream-accept-json-rejected.md)
- [guardrail-6.8 — SSE from stream endpoint](transports/guardrail-6.8-sse-from-stream-endpoint.md)
- [positive-6.9 — SSE event stream](transports/positive-6.9-sse-event-stream.md)
- [negative-6.10 — Stream rejects timeout/collect params](transports/negative-6.10-stream-rejects-timeout-collect-params.md)

### flows/ — multi-step end-to-end journeys (full inline transcripts)
- [flow-7.1 — Publish → subscribe → consume → ack](flows/flow-7.1-publish-subscribe-consume-ack.md)
- [flow-7.2 — Two-consumer rebalance](flows/flow-7.2-two-consumer-rebalance.md)
- [flow-7.3 — PositionsNotSet recovery](flows/flow-7.3-positions-not-set-recovery.md)

### errors/ — RFC-9457 envelope per common code
- [errors-8.1 — Problem Details envelope](errors/errors-8.1-problem-details-envelope.md)
- [errors-8.2 — 401 unauthenticated](errors/errors-8.2-401-unauthenticated.md)
- [errors-8.3 — 403 unauthorized](errors/errors-8.3-403-unauthorized.md)
- [errors-8.4 — 404 not found](errors/errors-8.4-404-not-found.md)
- [errors-8.5 — 409 conflict](errors/errors-8.5-409-conflict.md)
- [errors-8.6 — 412 sequence violation](errors/errors-8.6-412-sequence-violation.md)
- [errors-8.7 — 429 rate limited](errors/errors-8.7-429-rate-limited.md)
- [errors-8.8 — 500 internal](errors/errors-8.8-500-internal.md)

> Full catalog: **59 scenarios** across 8 areas. The SDK-orchestration behaviors that are *not* API scenarios (consumer-seek §5.7 and the SDK halves of §5.4–§5.6) live in the `event-broker-sdk-scenarios` change.

---

## 3. Rules & Legend

### Authoring rules

Every scenario MUST:

- **Assert only direct API interactions** — what a raw HTTP client (curl, the SDK, a future gRPC bridge) sends to the broker and observes back, plus broker-side state observable through a later HTTP call or the side-effects vocabulary. A scenario MUST NOT assert SDK internals (when the SDK re-SEEKs, how it diffs assignments, `OffsetManager` resolution, recovery loops, "the SDK does X"). SDK behavior is the contract of `event-broker-sdk-scenarios`, a separate change.
- **Assert both halves of the contract** — response semantics (status, headers, body) AND side effects (stored state, emitted frames, audit/metrics). A scenario MAY omit `## Side effects` only for pure-introspection endpoints (`GET /v1/topics`, `GET /v1/topics/segments`, `GET /v1/consumer_groups`) that change no state.
- **Use literal HTTP** in the `## Request` section — real method, path, headers, and JSON body inside an `http` fenced block. Placeholders use `<...>` (e.g., `<tenant-token>`); variables resolved by a `## Setup` step use `{name}` (e.g., `{sub_id}`).
- **Single-endpoint scenarios reference setup** — `## Setup` links to other scenarios for precondition steps and names the variables they yield (keeps each file focused on one exchange).
- **Flow scenarios are full inline transcripts** — `flows/*` show the entire sequence of HTTP exchanges in order, numbered (`## Exchange 1`, `## Exchange 2`, …), every request and response inline. They do NOT delegate steps to `## Setup`; the point is to read the whole client↔broker dialogue top to bottom. (Single-endpoint scenarios stay the normative per-call reference; flows are the narrative end-to-end view.)
- **Draw side effects from the predicate vocabulary** (below). A scenario needing a predicate not listed adds it here in the same change.

Authority split (see the change's design D5): `openapi.yaml` is authoritative for request/response **shape**; scenarios are authoritative for **semantics** (status codes, side effects, behavior). A scenario MUST NOT contradict the schema on shape.

### Side-effects predicate vocabulary

Side-effect bullets are one of these kinds:

| Kind | Form |
|---|---|
| State | `<table>(<key>) is set to <value>` · `<table>(<key>) is absent` · `<table>(<key>) advances from <old> to <new>` |
| Frame | `subscription <id> next frame is <kind> with <assertion>` · `subscription <id> emits <kind> within <duration>` |
| Reply | `subsequent <call> returns <code>` |
| Lifecycle | `subscription <id> is reaped after <duration>` · `consumer-group <id> is deleted` |
| Metric / audit | `metric <name> incremented by <n>` · `audit log entry <type> created` |

Concrete state tables: `evbk_group_offsets` (cursor; last-processed-offset semantics), `evbk_consumer_group` (group registry), `evbk_subscription` (cache-only), `evbk_event` (storage backend), producer-state (chained mode), idempotency-key set.

### Legend

| Shorthand | Meaning |
|---|---|
| `PD` | RFC-9457 Problem Details body (`application/problem+json`). Implied by any `4xx` / `5xx` unless stated otherwise. |
| `RF` | Partition **retention floor** — smallest offset still readable. |
| `HWM` | Partition **high-water mark** — offset of the next event to be admitted. |
| `MP` | Multipart frame on `:stream` (`multipart/mixed` part). |
| `SSE` | Server-Sent Event frame on `:sse`. |
| `Cursor` | Value in `evbk_group_offsets(group, topic, partition)` — last-processed offset; broker emits from `Cursor + 1`. |

### Conventions

- All scenarios run as tenant `T` with a valid `Authorization: Bearer <tenant-token>` unless the scenario explicitly tests auth. Token issuance is covered in operational docs, not scenarios.
- Topic / event-type / subject-type identifiers are full GTS strings (ASCII).
- Cursor values use last-processed-offset semantics throughout (an integer `N` means "consumed through `N`; deliver from `N + 1`"). See [features/0002](../docs/features/0002-consumer-subscription-lifecycle.md) §2.2.
