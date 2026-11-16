# Event Broker — Scenario Guide

Practical companion to [DESIGN.md](../docs/DESIGN.md) and [openapi.yaml](../docs/openapi.yaml). Each scenario is a concrete HTTP exchange: a literal request, the expected response, and the side effects the broker must produce.

Organized in two parts:

1. **How To** — the integration journey and per-area reference with mechanism summaries.
2. **Guardrails** — rejections, negative cases, and error shapes.

---

## 1. How To

### Three partition counts

"Partition" means one of three counts. They are independent: for the same topic they differ, and none constrains the others. For example, the producer has 4 partitions, ingest has 8 and the backend has 6.

- **Producer partitions** - the producer's own outbox partitions (the SDK's outbox producer). The producer chooses how to number its events: one chain across the whole topic, or one producer id per producer partition, which is what the SDK outbox registers. The broker never sees producer partitions.
- **Ingest partitions** - the partitions of the broker's ingest outbox group (`outbox_groups.default.partitions`, a power of 2 from 1 to 64). An event goes to the ingest partition that `{topic}:{key}` hashes to, so events of one key keep their order inside the broker.
- **Backend partitions** - the partitions of the topic's log in the storage backend. The backend assigns each event the backend partition its partition-key value hashes to. Consumers read these: assignment, SEEK, cursors, segments, `RF` and `HWM` are per backend partition, and so is the `partition` field on a read event. Once the topic is provisioned, the backend's count is the one in force.

The producer chain (`last_sequence`, dedup) is kept per `(producer, topic)`; no partition of any kind is part of it. Consumer scenarios that say "partition" mean a backend partition.

### Integration Journey

To publish and consume events through the broker, follow these steps. Each step links to the scenario(s) with full coverage.

#### Step 1 — Establish a consumer group

Anonymous groups are broker-minted via REST. Named groups are registered via `types_registry` at startup - no REST call needed (not yet implemented, tracked in #5702).

- [Create anonymous group](consumer/groups/1.01-positive-create-anonymous-group.md) - `POST /v1/consumer-groups` → broker mints GTS id; distribute it to your consumer fleet out-of-band; only the principal that created the group (its subject, in its tenant) may JOIN it, so the fleet runs as that principal.
- [JOIN a named group](consumer/groups/1.08-positive-named-group-join.md) - no creation step; JOIN with the well-known GTS identifier directly (not yet implemented, tracked in #5702).

#### Step 2 — Publish events

Producers submit typed events to a topic. Publish is async (`202`); opt into synchronous persistence with the standard `Prefer: wait=N` header (RFC 7240).

- [Publish a single event (async)](producer/single/1.01-positive-publish-single-async.md) — `POST /v1/events` → `202 Accepted`.
- [Publish sync (Prefer: wait=N)](producer/single/1.02-positive-publish-sync-wait-persisted.md) - `POST /v1/events` with `Prefer: wait=N` → `201 Created` once stored, `202` on timeout.

#### Step 3 — JOIN a subscription

A consumer instance JOINs the group with topic interests and receives its partition assignment.

- [Cold JOIN, fresh group](consumer/subscriptions/1.01-positive-cold-join-fresh-group.md) — `POST /v1/subscriptions` → `201` with `assigned[]` + `topology_version`.

#### Step 4 — SEEK the starting position

Before streaming, the consumer declares where each assigned partition begins. **This step is required** — opening the stream without it returns `409 Failed Precondition`.

**Path A — Consumer has a persistent store (service DB, browser IndexedDB, filesystem)**

Read the last processed offset from your own store and SEEK with the exact integer:

- [SEEK to exact offset](consumer/positions/1.03-positive-seek-exact-offset.md) — `POST /v1/subscriptions/{id}:seek` with integer offset from own DB.
- [SEEK to timestamp](consumer/positions/1.11-positive-seek-at-timestamp.md) — use `"at:<ISO-8601>"` sentinel for date-anchored readers; response returns resolved integer for persistence.
- [Full Path A journey](consumer/flows/1.03-flow-path-a-consumer-with-db.md) — end-to-end: JOIN → SEEK from DB → stream → persist offset → reconnect resumes correctly.

**Path B — No persistent store (one-shot scripts, stateless readers)**

SEEK with a sentinel; accept reprocessing on restart:

- [SEEK to earliest](consumer/positions/1.01-positive-seek-earliest.md) — `"earliest"` → cursor = RF − 1 (= 0 on a fresh topic where RF = 1; always ≥ 0); broker emits from RF.
- [SEEK to latest](consumer/positions/1.02-positive-seek-latest.md) — `"latest"` → broker resolves to current HWM; only future events delivered.

#### Step 5 — Stream events

Open the long-lived multipart stream and consume frames as they arrive.

- [Stream multipart frames](consumer/stream/1.01-positive-stream-multipart-frames.md) — `GET /v1/events:stream` → `200 multipart/mixed`; one event per part.
- [SSE event stream](consumer/stream/1.09-positive-sse-event-stream.md) — `GET /v1/events:sse` → `200 text/event-stream`; browser-native alternative.
- [Wildcard event-type stream](consumer/stream/1.15-positive-wildcard-event-type-stream.md) — interest `types: ["gts.cf.core.events.event.v1~*"]` (a topic-only subscription) delivers a concrete-type event.
- [Subject-type selector](consumer/stream/1.18-positive-subject-types-selector.md) - interest `subject_types` delivers only events whose `subject_type` matches; a delivery filter, not an access grant.
- [Exact type selector](consumer/stream/1.21-positive-exact-type-selector.md) - an exact type id selects only that type; a sibling type on the topic is not delivered.
- [Base type selector](consumer/stream/1.22-positive-base-type-selector-covers-derived-types.md) - a bare type id covers its derived types: the base and both derived types are delivered, a type of another family is not.
- [Two levels below the selector](consumer/stream/1.23-positive-selector-covers-two-levels-below.md) - a bare type id covers derived types at any depth.
- [Derived type selector](consumer/stream/1.24-positive-derived-type-selector.md) - a derived type id covers its own subtree, not its base or a sibling.
- [Prefix wildcard selector](consumer/stream/1.25-positive-prefix-wildcard-selector.md) - a namespace with a trailing `*` delivers the types under it, not an outsider on the same topic.
- [Selector without a minor version](consumer/stream/1.26-positive-selector-covers-minor-versions.md) - `v1` covers `v1.0` and `v1.1`, not `v2`; the selector itself need not be registered.
- [Selector matching no type](consumer/stream/1.27-positive-selector-matching-no-type.md) - JOIN `201` lists the entry in `unmatched_types`; the stream delivers the interest nothing.
- [Overlapping selectors](consumer/stream/1.28-positive-overlapping-selectors-deliver-once.md) - an event two entries of one interest match is delivered as one frame.
- [Type admitted after the JOIN](consumer/stream/1.30-positive-type-admitted-after-join.md) - a derived type admitted after the JOIN is delivered once the broker resolves it, decided by its own `consume` answer.
- [Base subject-type selector](consumer/stream/1.32-positive-base-subject-type-selector.md) - a bare subject type id covers its derived subject types.
- [Trailing-wildcard subject-type selector](consumer/stream/1.33-positive-trailing-wildcard-subject-selector.md) - `<base>~acme.*` covers derived subject types under that vendor only, not the bare base or another vendor.
- [Subject-type selector matching no subject type](consumer/stream/1.34-positive-subject-selector-matching-no-subject.md) - JOIN `201` with `unmatched_types: []`; the stream delivers the interest nothing.

> **End-to-end**: [Publish → subscribe → consume](flows/1.01-flow-publish-subscribe-consume.md) composes all steps into one coupled transcript.

---

### Parallel consumer group formation

Multiple consumer instances share a group by JOINing with the same `consumer_group` identifier. The broker rebalances partitions across all active members automatically.

**Anonymous group**: one instance calls `POST /v1/consumer-groups` and distributes the returned id out-of-band (shared DB row, ConfigMap, env var). All instances then JOIN with that id, each running as the principal that created the group (its subject, in its tenant): only that principal may JOIN it (`403 ConsumerGroupNotOwned`).

**Named group**: no creation step. All instances JOIN with the well-known GTS identifier - no coordination needed (not yet implemented, tracked in #5702).

- [Second JOIN triggers rebalance](consumer/subscriptions/1.11-positive-second-join-triggers-rebalance.md) — 2nd member joins; `topology` frame splits 4 partitions 2+2.
- [Third JOIN triggers rebalance](consumer/subscriptions/1.12-positive-third-join-triggers-rebalance.md) — 3rd member joins; three-way partition split.

---

### producer/ — Publish events

#### producer/single/ — `POST /v1/events` (stateless)
- [positive-1.1 — Publish single (async)](producer/single/1.01-positive-publish-single-async.md) — `POST /v1/events` → `202 Accepted`; event enqueued in outbox.
- [positive-1.2 - Publish sync (Prefer: wait=N)](producer/single/1.02-positive-publish-sync-wait-persisted.md) - `POST /v1/events` with `Prefer: wait=N` → `201 Created` after backend persist, `202 Accepted` if the wait elapses first.
- [negative-1.5 — Read-only partition rejected](producer/single/1.05-negative-readonly-partition-rejected.md) — producer-supplied `partition` on publish → `400 Bad Request`; the backend assigns the backend partition itself.
- [negative-1.6 - Malformed `Prefer: wait`](producer/single/1.06-negative-prefer-wait-malformed.md) - a non-numeric `Prefer: wait` value → `400 Bad Request`; parsed before the write path, so nothing is enqueued. A numeric over-cap value is **not** malformed - it clamps to the cap (see [single/1.02](producer/single/1.02-positive-publish-sync-wait-persisted.md)).
- [negative-1.7 - Malformed trace_parent](producer/single/1.07-negative-invalid-trace-parent.md) - a `trace_parent` that is not a W3C traceparent → `400 Invalid Argument`; nothing is admitted.
- [negative-1.8 - Event too large](producer/single/1.08-negative-event-too-large.md) - encoding over 60 KiB → `413 Content Too Large` (`event_too_large`); nothing is admitted.
- [negative-1.9 - Partition key too long](producer/single/1.09-negative-partition-key-too-long.md) - resolved partition key over 1024 bytes → `400 Invalid Argument` (`partition_key_too_long`); nothing is admitted.
- [negative-1.13 - Nil event tenant](producer/single/1.13-negative-publish-nil-tenant.md) - an event whose `subject_tenant` is the nil UUID → `400 Invalid Argument` (`InvalidTenant`) before any authorization call; nothing is admitted.

#### producer/batch/ — `POST /v1/events:batch`
- [positive-1.1 — Publish batch](producer/batch/1.01-positive-publish-batch.md) — `POST /v1/events:batch` → `202 Accepted`; one topic per batch, all-or-nothing.
- [positive-1.9 - Batch of only duplicates](producer/batch/1.09-positive-batch-all-duplicates.md) - lost-ack resend of a whole chained run → `200 OK`, no body; the cursor stays and each event is stored once.
- [positive-1.10 - Batch sync (Prefer: wait=N)](producer/batch/1.10-positive-batch-sync-wait.md) - `POST /v1/events:batch` with `Prefer: wait=N` → `201 Created` once every admitted event is stored, `202 Accepted` on timeout, `200 OK` when all duplicates; the hold is on the admitted events only (a duplicate prefix is not waited on).
- [negative-1.11 - Batch with an event too large](producer/batch/1.11-negative-batch-event-too-large.md) - one event over 60 KiB → `413 Content Too Large` (`event_too_large`, `events[1]`); nothing stored.
- [negative-1.12 - Batch with a partition key too long](producer/batch/1.12-negative-batch-partition-key-too-long.md) - one partition key over 1024 bytes → `400 Invalid Argument` (`partition_key_too_long`, `events[1].partition_key`); nothing stored.
- [negative-1.13 - Batch with an event in an unreachable tenant](producer/batch/1.13-negative-batch-tenant-not-authorized.md) - one event for a tenant the caller cannot reach → `403 Permission Denied` (`TenantIdNotAuthorized`); nothing stored, including the events around it.
- [negative-1.14 - Batch mode-shape violation](producer/batch/1.14-negative-batch-mode-shape-violation.md) - one chained event without `meta.previous` → `400 ChainModeFieldsMissing` for the whole batch; nothing is admitted.

#### producer/flows/ — `POST /v1/producers`, `GET /cursors`, `POST :reset`
- [positive-1.1 - Register chained producer](producer/flows/1.01-positive-register-chained-producer.md) - `POST /v1/producers { mode: chained, client_agent }` → `201` with `id`.
- [positive-1.2 - Register monotonic producer](producer/flows/1.02-positive-register-monotonic-producer.md) - `POST /v1/producers { mode: monotonic, client_agent }` → `201`.
- [positive-1.3 - Chained-mode sequence](producer/flows/1.03-positive-chained-mode-sequence.md) - `POST /v1/events` with `meta.producer` + `meta.previous/sequence`; broker deduplicates.
- [positive-1.4 - Chain-head retry dedup](producer/flows/1.04-positive-chain-head-retry-dedup.md) - a resend at the current chain head (`meta.sequence == last_sequence`, whatever its `meta.previous`) returns `200` with an empty body; no second write. Dedup keys on `sequence`, not on the event `id`.
- [positive-1.6 - Cursor recovery](producer/flows/1.06-positive-cursor-recovery.md) - `GET /v1/producers/{id}/cursors` → `{id, client_agent, topics:[{topic, last_sequence}]}`; re-seeds the producer's chain per topic after desync.
- [positive-1.7 — Chain reset](producer/flows/1.07-positive-chain-reset.md) — `POST /v1/producers/{id}:reset` → `200`; chain state cleared, audited.
- [flow-1.9 — Desync recovery](producer/flows/1.09-flow-chained-producer-desync-recovery.md) — producer loses sequence state; first publish fails `412`; reads `/cursors`; reconciles counter; republishes successfully.
- [positive-1.10 - Monotonic-mode gap accepted](producer/flows/1.10-positive-monotonic-mode-sequence.md) - monotonic producer sends `meta.sequence` above `last_sequence` without `meta.previous` → `202 Accepted`; a gap is allowed and `last_sequence` takes the submitted value.
- [negative-1.11 - Monotonic sequence not advanced](producer/flows/1.11-negative-monotonic-sequence-not-advanced.md) - monotonic producer sends `meta.sequence` at or below `last_sequence` → a duplicate: no second event is written, `last_sequence` stays, no `412`.
- [negative-1.12 - Monotonic publish carrying previous](producer/flows/1.12-negative-monotonic-publish-carries-previous.md) - monotonic producer sends `meta.previous` → `400 MonotonicModeFieldsViolation`; nothing is admitted.
- [negative-1.13 - Chained publish without previous](producer/flows/1.13-negative-chained-publish-missing-previous.md) - chained producer omits `meta.previous` → `400 ChainModeFieldsMissing`; nothing is admitted.
- [negative-1.14 - Chained publish without sequence](producer/flows/1.14-negative-chained-publish-missing-sequence.md) - chained producer omits `meta.sequence` → `400 ChainModeFieldsMissing`; nothing is admitted.
- [negative-1.15 - Monotonic publish without sequence](producer/flows/1.15-negative-monotonic-publish-missing-sequence.md) - monotonic producer omits `meta.sequence` → `400 MonotonicModeFieldsViolation`; nothing is admitted.
- [negative-1.16 - Meta without producer](producer/flows/1.16-negative-meta-without-producer.md) - `meta.previous` or `meta.sequence` without `meta.producer` → `400 MetaWithoutProducerId`; nothing is admitted.
- [negative-1.17 - Unknown meta version](producer/flows/1.17-negative-unknown-meta-version.md) - `meta.version` above `1` → `400 UnknownMetaVersion`; nothing is admitted.
- [positive-1.18 - Version-only meta is stateless](producer/flows/1.18-positive-meta-with-only-version-is-stateless.md) - `meta` at a supported version with neither producer nor chain field → `202 Accepted`, stored as a stateless event; no chain.

---

### consumer/ — Consume events

#### consumer/groups/ — `POST/GET/DELETE /v1/consumer-groups`
- [positive-1.1 — Create anonymous group](consumer/groups/1.01-positive-create-anonymous-group.md) — `POST /v1/consumer-groups` → `201` with broker-minted GTS id.
- [positive-1.2 — Get group by id](consumer/groups/1.02-positive-get-group-by-id.md) — `GET /v1/consumer-groups/{id}` → full group record.
- [positive-1.3 — List groups](consumer/groups/1.03-positive-list-groups.md) — `GET /v1/consumer-groups` → paged list of caller-visible groups.
- [positive-1.4 — Delete empty group](consumer/groups/1.04-positive-delete-empty-group.md) — `DELETE /v1/consumer-groups/{id}` → `204`; only when no active subscriptions.
- [negative-1.6 - Invalid client_agent](consumer/groups/1.06-negative-invalid-client-agent.md) - non-ASCII, oversized or off-grammar `client_agent` → `400 Invalid Argument`.
- [positive-1.8 - Named group JOIN (no create step)](consumer/groups/1.08-positive-named-group-join.md) - JOIN with `types_registry`-provisioned identifier; broker validates `:consume` grant (not yet implemented, tracked in #5702).
- [negative-1.9 - Anonymous group access from another tenant](consumer/groups/1.09-negative-cross-tenant-anonymous-group-ownership.md) - the group belongs to its creating principal in its tenant; a principal of a tenant the policy does not let reach the group's tenant: GET and DELETE → `403 Permission Denied` (`TenantIdNotAuthorized`), LIST leaves the group out, JOIN → `403 Permission Denied` (`ConsumerGroupNotOwned`) before any policy call.
- [negative-1.10 - DELETE by another principal](consumer/groups/1.10-negative-delete-anonymous-group-not-owned.md) - a principal other than the anonymous group's creator in its tenant (another subject of that tenant, or the creator's subject in another tenant) sends `DELETE /v1/consumer-groups/{id}` → `403 Permission Denied` (`ConsumerGroupNotOwned`) after the tenant check, before the active-members check; the group stays.

#### consumer/subscriptions/ — `POST/GET/DELETE /v1/subscriptions`
- [positive-1.1 — Cold JOIN, fresh group](consumer/subscriptions/1.01-positive-cold-join-fresh-group.md) — `POST /v1/subscriptions` → `201` with `assigned[]` and `topology_version`.
- [positive-1.2 — Multi-topic interests](consumer/subscriptions/1.02-positive-join-multi-topic-interests.md) — JOIN with interests across two topics; partition assignment spans both.
- [positive-1.3 — Typed filter](consumer/subscriptions/1.03-positive-join-with-typed-filter.md) — `interest.filter { engine, expression }` applied per-member at delivery.
- [positive-1.4 — Multiple subscriptions (parallelism)](consumer/subscriptions/1.04-positive-parallelism-multiple-subscriptions.md) — second JOIN rebalances partitions 2+2.
- [positive-1.5 — Leave subscription](consumer/subscriptions/1.05-positive-leave-subscription.md) — `DELETE /v1/subscriptions/{id}` → `204`; triggers rebalance.
- [negative-1.6 - Unauthorized topic JOIN](consumer/subscriptions/1.06-negative-join-unauthorized-topic.md) - interest references a topic the principal cannot consume → `403`.
- [positive-1.9 — List subscriptions](consumer/subscriptions/1.09-positive-list-subscriptions.md) — `GET /v1/subscriptions` → paged list of active subscriptions.
- [positive-1.10 — Read subscription](consumer/subscriptions/1.10-positive-read-subscription.md) — `GET /v1/subscriptions/{id}` → current assignment and expiry.
- [positive-1.11 — Second JOIN triggers rebalance](consumer/subscriptions/1.11-positive-second-join-triggers-rebalance.md) — 2nd instance joins; partitions split 2+2; `topology` frame emitted.
- [positive-1.12 — Third JOIN triggers rebalance](consumer/subscriptions/1.12-positive-third-join-triggers-rebalance.md) — 3rd instance joins; three-way split; all streams updated.
- [negative-1.13 - JOIN group at capacity](consumer/subscriptions/1.13-negative-join-group-at-capacity.md) - all partitions already assigned; new JOIN → `429` with `Retry-After`; zero-partition standby not admitted.
- [positive-1.14 - JOIN without an ownership filter](consumer/subscriptions/1.14-positive-join-only-mine.md) - an interest naming neither `ownership_filter` nor `subject_tenant` → `201` echoing `only_mine` and the caller's tenant; it reads only events whose `subject_owner` is the caller, never unowned ones (those need `entire_tenant`).
- [negative-1.15 - JOIN sending max_depth](consumer/subscriptions/1.15-negative-join-max-depth-rejected.md) - an interest that sends `max_depth` → `400 Invalid Argument` (`InvalidBody`) naming `ownership_filter`; no subscription created.
- [negative-1.16 - JOIN by another principal](consumer/subscriptions/1.16-negative-join-anonymous-group-not-owned.md) - a principal other than the anonymous group's creator in its tenant (another subject of that tenant, or the creator's subject in another tenant) JOINs it → `403 Permission Denied` (`ConsumerGroupNotOwned`) before any policy call; no subscription is created and the group's members stay.
- [negative-1.17 - JOIN, stream or SEEK without consume on the group](consumer/subscriptions/1.17-negative-join-consumer-group-not-authorized.md) - a principal the policy refuses `consume` on the consumer group JOINs it, or opens `:stream` or SEEKs once that grant is revoked → `403 Permission Denied` (`ConsumerGroupNotAuthorized`); no subscription, lease or cursor (only an action-aware policy refuses it).
- [positive-1.18 - JOIN naming an unregistered type](consumer/subscriptions/1.18-positive-join-unregistered-type-reported.md) - an interest names a type the topic has not registered → `201`, the entry listed in that interest's `unmatched_types` (JOIN response only; exact entries only above 256 types on the topic).
- [negative-1.19 - JOIN only_mine for another tenant](consumer/subscriptions/1.19-negative-join-only-mine-foreign-tenant.md) - an `only_mine` interest naming a `subject_tenant` other than the caller's → `400 Invalid Argument` (`InvalidTenant`) before any authorization call; no subscription created.
- [negative-1.20 - Nil interest tenant](consumer/subscriptions/1.20-negative-join-nil-interest-tenant.md) - an interest whose `subject_tenant` is the nil UUID → `400 Invalid Argument` (`InvalidTenant`) before any authorization call; no subscription is created.
- [negative-1.21 - Too many types](consumer/subscriptions/1.21-negative-join-too-many-types.md) - more than 32 `types` in one interest → `400 Invalid Argument` (`TooManyTypes`) before any authorization call; no subscription is created.
- [negative-1.22 - Too many subject types](consumer/subscriptions/1.22-negative-join-too-many-subject-types.md) - more than 32 `subject_types` in one interest → `400 Invalid Argument` (`TooManySubjectTypes`) before any authorization call; no subscription is created.
- [negative-1.23 - LEAVE by another principal](consumer/subscriptions/1.23-negative-leave-subscription-not-owned.md) - a principal that did not JOIN the subscription, even one of its tenant, sends `DELETE /v1/subscriptions/{id}` → `403 Permission Denied` (`SubscriptionNotOwned`); the subscription stays.
- [negative-1.24 - JOIN for an exact subject type the scope excludes](consumer/subscriptions/1.24-negative-join-subject-type-not-authorized.md) - an exact `subject_types` entry that overlaps a subject type a matched registered event type allows (one covers the other), and that the consume scope excludes → `403 Permission Denied` (`SubjectTypeNotAuthorized`), naming the subject type; no subscription is created.

#### consumer/positions/ — `POST /v1/subscriptions/{id}:seek` (SEEK)
- [positive-1.1 — SEEK earliest](consumer/positions/1.01-positive-seek-earliest.md) — `"earliest"` → cursor = RF − 1 (= 0 on a fresh topic where RF = 1; always ≥ 0); broker emits from RF onwards.
- [positive-1.2 — SEEK latest](consumer/positions/1.02-positive-seek-latest.md) — `"latest"` → cursor set to HWM; only future events delivered.
- [positive-1.3 — SEEK exact offset](consumer/positions/1.03-positive-seek-exact-offset.md) — integer last-processed offset from own DB; broker emits from `offset + 1`.
- [positive-1.4 — Mixed sentinels and integers](consumer/positions/1.04-positive-mixed-sentinels.md) — partitions may mix `"earliest"`, `"latest"`, and exact offsets in one request.
- [positive-1.10 — Any value in retention range](consumer/positions/1.10-positive-seek-any-value-in-range.md) — any integer in `[RF−1, HWM]` is accepted.
- [positive-1.11 — SEEK at timestamp](consumer/positions/1.11-positive-seek-at-timestamp.md) — `"at:<ISO-8601>"` → resolves to first event at or after timestamp; response returns integer.
- [positive-1.12 — Timestamp before retention](consumer/positions/1.12-positive-seek-at-timestamp-before-retention.md) — timestamp before RF → clamps to RF.
- [positive-1.13 — Timestamp beyond HWM](consumer/positions/1.13-positive-seek-at-timestamp-beyond-hwm.md) — future timestamp → resolves to HWM (equivalent to `"latest"`).
- [positive-1.14 — Resume from persisted cursor](consumer/positions/1.14-positive-seek-resume-from-cursor.md) — consumer reads to sequence 40, persists cursor, reconnects with SEEK to 40; first event on resumed stream is sequence 41.
- [negative-1.15 - SEEK at a stale topology version](consumer/positions/1.15-negative-seek-stale-topology-version.md) - a `topology_version` that differs from the subscription's → `412 Failed Precondition` (`topology_version_mismatch`) with no version or assignment in the body; nothing is seeded, and the client re-reads the subscription and re-seeks.
- [positive-1.16 - SEEK after a join-triggered rebalance](consumer/positions/1.16-positive-seek-after-join-rebalance.md) - a SEEK at the version observed before another member JOINed → `412`; after re-reading the subscription, the re-seek of a kept partition at the current `topology_version` → `200`.
- [positive-1.17 - SEEK after a leave-triggered rebalance](consumer/positions/1.17-positive-seek-after-leave-rebalance.md) - a SEEK at the version observed before a member LEAVEs → `412`; after re-reading the subscription, the re-seek of a partition it already held at the current `topology_version` → `200`.

#### consumer/stream/ — `GET /v1/events:stream`, `GET /v1/events:sse`
- [positive-1.1 — Multipart frames](consumer/stream/1.01-positive-stream-multipart-frames.md) — `GET /v1/events:stream` → `200 multipart/mixed`; `event`, `heartbeat`, `advisory`, `topology` frame kinds.
- [positive-1.2 — Heartbeat cadence](consumer/stream/1.02-positive-stream-heartbeat-cadence.md) — idle stream emits `heartbeat` every 5 s; keeps connection alive through proxies.
- [positive-1.3 — Topology frame on rebalance](consumer/stream/1.03-positive-stream-topology-frame-on-rebalance.md) — mid-stream JOIN by another member triggers `topology` frame with new `assigned[]`.
- [positive-1.9 — SSE event stream](consumer/stream/1.09-positive-sse-event-stream.md) — `GET /v1/events:sse` → `200 text/event-stream`; same frame schema as multipart.
- [positive-1.15 — Wildcard event-type stream](consumer/stream/1.15-positive-wildcard-event-type-stream.md) — interest with the all-types wildcard `gts.cf.core.events.event.v1~*` (the topic-only subscription shape) delivers a concrete-type event; the path that had no black-box coverage before GTS pattern matching.
- [positive-1.17 - Only-mine delivery](consumer/stream/1.17-positive-only-mine-delivery.md) - an `only_mine` interest is delivered only the caller's owned events; an unowned event and another owner's event are skipped, and an `entire_tenant` interest gets all three.
- [positive-1.18 - Subject-type selector](consumer/stream/1.18-positive-subject-types-selector.md) - interest `subject_types` (1-32 GTS type patterns) delivers only events whose `subject_type` matches; no selector delivers every subject type; no authorization meaning.
- [positive-1.19 - Subtree delivery](consumer/stream/1.19-positive-subtree-delivery.md) - a `tenant_subtree_included` interest rooted at the caller's tenant is delivered a child tenant's event, with its own `subject_tenant`, and not an outsider's.
- [positive-1.21 - Exact type selector](consumer/stream/1.21-positive-exact-type-selector.md) - an exact type id selects only that type; a sibling type on the topic is not delivered.
- [positive-1.22 - Base type selector](consumer/stream/1.22-positive-base-type-selector-covers-derived-types.md) - a bare type id covers its derived types: the base and both derived types are delivered, a type of another family is not.
- [positive-1.23 - Two levels below the selector](consumer/stream/1.23-positive-selector-covers-two-levels-below.md) - a bare type id covers derived types at any depth.
- [positive-1.24 - Derived type selector](consumer/stream/1.24-positive-derived-type-selector.md) - a derived type id covers its own subtree, not its base or a sibling.
- [positive-1.25 - Prefix wildcard selector](consumer/stream/1.25-positive-prefix-wildcard-selector.md) - a namespace with a trailing `*` delivers the types under it, not an outsider on the same topic.
- [positive-1.26 - Selector without a minor version](consumer/stream/1.26-positive-selector-covers-minor-versions.md) - `v1` covers `v1.0` and `v1.1`, not `v2`; the selector itself need not be registered.
- [positive-1.27 - Selector matching no type](consumer/stream/1.27-positive-selector-matching-no-type.md) - JOIN `201` lists the entry in `unmatched_types`; the stream delivers the interest nothing.
- [positive-1.28 - Overlapping selectors](consumer/stream/1.28-positive-overlapping-selectors-deliver-once.md) - an event two entries of one interest match is delivered as one frame.
- [negative-1.29 - Derived type the scope leaves out](consumer/stream/1.29-negative-derived-type-denied-by-scope.md) - a base selector is not delivered a derived type the consume scope does not list; the JOIN is not refused (only an action-aware policy shows it).
- [positive-1.30 - Type admitted after the JOIN](consumer/stream/1.30-positive-type-admitted-after-join.md) - a derived type admitted after the JOIN is delivered once the broker resolves it, decided by its own `consume` answer.
- [negative-1.31 - Late type denied](consumer/stream/1.31-negative-type-admitted-after-join-denied.md) - a type admitted after the JOIN whose own `consume` answer denies it is skipped (only an action-aware policy shows it).
- [positive-1.32 - Base subject-type selector](consumer/stream/1.32-positive-base-subject-type-selector.md) - a bare subject type id covers its derived subject types.
- [positive-1.33 - Trailing-wildcard subject-type selector](consumer/stream/1.33-positive-trailing-wildcard-subject-selector.md) - `<base>~acme.*` covers derived subject types under that vendor only, not the bare base or another vendor.
- [positive-1.34 - Subject-type selector matching no subject type](consumer/stream/1.34-positive-subject-selector-matching-no-subject.md) - JOIN `201` with `unmatched_types: []`; the stream delivers the interest nothing.
- [positive-1.12 - Partition gain terminates subscription](consumer/stream/1.12-positive-stream-terminates-on-rebalance-gain.md) (not yet answered as terminated, tracked in #5977) - surviving member gains freed partitions; broker sends `control/terminal`; consumer re-JOINs and re-SEEKs.
- [positive-1.13 — DELETE while streaming](consumer/stream/1.13-positive-delete-while-streaming.md) — `DELETE /v1/subscriptions/{id}` while stream is open closes the connection immediately; no final control frame emitted.
- [positive-1.14 — Control progress frame](consumer/stream/1.14-positive-control-progress-frame.md) — heavily-filtered stream emits `control/progress` so the consumer can advance its offset store without re-scanning.

#### consumer/flows/ — consumer-only end-to-end journeys
- [flow-1.1 — Two-consumer rebalance](consumer/flows/1.01-flow-two-consumer-rebalance.md) — full inline transcript: consumer A holds all partitions; B joins; rebalance; both stream.
- [flow-1.2 - PositionsNotSet recovery](consumer/flows/1.02-flow-positions-not-set-recovery.md) - SDK mis-SEEKs; broker returns `409`; SDK re-JOINs, re-SEEKs and resumes.
- [flow-1.3 — Path A consumer with DB](consumer/flows/1.03-flow-path-a-consumer-with-db.md) — consumer reads own DB → SEEK exact offset → stream → persist offset → reconnect resumes from correct position.
- [flow-1.4 - Leave triggers gain then terminate](consumer/flows/1.04-flow-leave-triggers-gain-terminate.md) (not yet answered as terminated, tracked in #5977) - B leaves; A gains B's partitions; A receives `control/terminal` and re-JOINs with the full assignment.

---

### topics/ — Topic introspection

- [positive-1.1 — List topics](topics/1.01-positive-list-topics.md) — `GET /v1/topics` → paged list of topic DTOs carrying `id`, `description`, `retention`.
- [positive-1.2 — List topic segments](topics/1.02-positive-list-topic-segments.md) — `GET /v1/topics/segments?topic=...&partition=...` → segment manifest with RF/HWM per segment.
- [positive-1.5 - Segments of an empty partition](topics/1.05-positive-segments-empty-partition.md) - `GET /v1/topics/segments?topic=...&partition=...` for a partition with no stored events → `200 OK` with `start_sequence: 0`, `end_sequence: 0`, null times and empty `segments[]`.
- [positive-1.4 - List event types](topics/1.04-positive-list-event-types.md) - `GET /v1/event-types?$filter=topic eq '...'` → paged list of event type registrations.

---

### flows/ — Coupled producer + consumer journeys

- [flow-1.1 — Publish → subscribe → consume](flows/1.01-flow-publish-subscribe-consume.md) — producer publishes 3 events; consumer creates group, JOINs, SEEKs, streams, processes.

---

## 2. Guardrails

### Auth & permissions

- [negative-1.1 — Missing bearer token](auth/1.01-negative-missing-bearer-token.md) — no `Authorization` header → `401 Unauthenticated` on any endpoint.
- [negative-1.2 — Invalid bearer token](auth/1.02-negative-invalid-bearer-token.md) — expired or malformed token → `401 Unauthenticated`.
- [negative-1.3 - No produce permission](auth/1.03-negative-insufficient-permission-produce.md) - `POST /v1/events` without a produce grant → `403 Permission Denied` (`TenantIdNotAuthorized`).
- [negative-1.4 - No consume permission](auth/1.04-negative-insufficient-permission-consume.md) - the anonymous group's creator sends `POST /v1/subscriptions` without any consume grant → refused by the consumer-group check → `403 Permission Denied` (`ConsumerGroupNotAuthorized`); no subscription is created (only an action-aware policy refuses it).
- [negative-1.5 - Cross-tenant anonymous group](auth/1.05-negative-cross-tenant-anonymous-group.md) - the group belongs to its creating principal in its tenant; a principal of tenant B JOINs tenant A's anonymous group → `403 Permission Denied` (`ConsumerGroupNotOwned`) before any policy call.
- [negative-1.6 - Tenant outside the subtree](auth/1.06-negative-tenant-outside-subtree.md) - `POST /v1/events` for a tenant the caller cannot reach and holds no barrier right for → `403 Permission Denied` (`TenantIdNotAuthorized`); nothing is enqueued.
- [positive-1.7 - Publish across a barrier with the grant](auth/1.07-positive-publish-across-barrier-with-grant.md) - `POST /v1/events` for a self-managed tenant below the caller, with `produce_ignore_barrier` on the event type → `202 Accepted`.
- [negative-1.8 - JOIN for a tenant outside the subtree](auth/1.08-negative-join-tenant-outside-subtree.md) - `POST /v1/subscriptions` with an interest tenant the caller cannot reach → the consume call and the tenant check are denied → `403 Permission Denied` (`TenantIdNotAuthorized`); no subscription is created.
- [negative-1.9 - JOIN across a barrier without the right](auth/1.09-negative-join-barrier-ignore-without-grant.md) - an interest with `barrier_mode: ignore` and no `consume_ignore_barrier` on the topic → `403 Permission Denied` (`TenantIdNotAuthorized`); no subscription is created.
- [negative-1.10 - JOIN for an exact type the scope excludes](auth/1.10-negative-join-event-type-excluded-by-scope.md) - an exact type pattern whose registered types the consume scope admits none of → `403 Permission Denied` (`EventTypeNotAuthorized`), naming the pattern; no subscription is created.
- [negative-1.11 - Management by a caller with no identity](auth/1.11-negative-nil-caller-management.md) - a caller whose subject or tenant is the nil UUID lists, creates, reads, deletes, JOINs, LEAVEs, SEEKs or opens a stream → `403 Permission Denied` (`TenantIdNotAuthorized`) before any lookup or policy call; a list answers 403, not an empty page; nothing changes.
- [negative-1.12 - Segments for a topic the caller may not read](auth/1.12-negative-segments-topic-not-authorized.md) - `GET /v1/topics/segments` by a caller the policy lets neither consume the topic nor publish any of its event types → `403 Permission Denied` (`TopicNotAuthorized`) before the topic lookup, for a registered and an unregistered topic alike.
- [negative-1.13 - Producer registration the tenant check refuses](auth/1.13-negative-producer-registration-tenant-not-authorized.md) - `POST /v1/producers` by a caller the policy refuses `produce` at its own tenant → `403 Permission Denied` (`TenantIdNotAuthorized`); no producer is stored.
- [negative-1.6 — Unauthorized topic JOIN](consumer/subscriptions/1.06-negative-join-unauthorized-topic.md) — interest references a topic the principal cannot consume → `403`.
- [negative-1.24 - JOIN for an exact subject type the scope excludes](consumer/subscriptions/1.24-negative-join-subject-type-not-authorized.md) - an exact `subject_types` entry that overlaps a subject type a matched registered event type allows (one covers the other), and that the consume scope excludes → `403 Permission Denied` (`SubjectTypeNotAuthorized`), naming the subject type; no subscription is created.
- [negative-1.20 - Stream by another principal](consumer/stream/1.20-negative-stream-subscription-not-owned.md) - a principal that did not JOIN the subscription, even one of its tenant, opens `:stream` → `403 Permission Denied` (`SubscriptionNotOwned`) after the tenant check, before the positions check and the lease; no stream opens.
- [negative-1.23 - LEAVE by another principal](consumer/subscriptions/1.23-negative-leave-subscription-not-owned.md) - a principal that did not JOIN the subscription, even one of its tenant, sends `DELETE /v1/subscriptions/{id}` → `403 Permission Denied` (`SubscriptionNotOwned`); the subscription stays.
- [negative-1.18 - SEEK by another principal](consumer/positions/1.18-negative-seek-subscription-not-owned.md) - a principal that did not JOIN the subscription, even one of its tenant, SEEKs it → `403 Permission Denied` (`SubscriptionNotOwned`) before the open-stream check; no cursor is written.
- [negative-1.16 - JOIN by another principal](consumer/subscriptions/1.16-negative-join-anonymous-group-not-owned.md) - a principal other than the anonymous group's creator in its tenant (another subject of that tenant, or the creator's subject in another tenant) JOINs it → `403 Permission Denied` (`ConsumerGroupNotOwned`) before any policy call; no subscription is created and the group's members stay.
- [negative-1.17 - JOIN, stream or SEEK without consume on the group](consumer/subscriptions/1.17-negative-join-consumer-group-not-authorized.md) - a principal the policy refuses `consume` on the consumer group JOINs it, or opens `:stream` or SEEKs once that grant is revoked → `403 Permission Denied` (`ConsumerGroupNotAuthorized`); no subscription, lease or cursor (only an action-aware policy refuses it).
- [negative-1.10 - DELETE by another principal](consumer/groups/1.10-negative-delete-anonymous-group-not-owned.md) - a principal other than the anonymous group's creator in its tenant (another subject of that tenant, or the creator's subject in another tenant) sends `DELETE /v1/consumer-groups/{id}` → `403 Permission Denied` (`ConsumerGroupNotOwned`) after the tenant check, before the active-members check; the group stays.

### Input validation

- [negative-1.3 — Schema validation failure](producer/single/1.03-negative-schema-validation-failure.md) — `event.data` fails JSON Schema → `422 Invalid Argument`.
- [negative-1.5 — Read-only partition rejected](producer/single/1.05-negative-readonly-partition-rejected.md) — producer-supplied `partition` on publish → `400 Bad Request`; `partition` is consumer-facing/read-side only.
- [negative-1.6 - Malformed `Prefer: wait`](producer/single/1.06-negative-prefer-wait-malformed.md) - a non-numeric `Prefer: wait` value → `400 Bad Request`; parsed before the write path, so nothing is enqueued.
- [negative-1.7 - Malformed trace_parent](producer/single/1.07-negative-invalid-trace-parent.md) - a `trace_parent` that is not a W3C traceparent → `400 Invalid Argument`; nothing is admitted.
- [negative-1.8 - Event too large](producer/single/1.08-negative-event-too-large.md) - encoding over 60 KiB → `413 Content Too Large` (`event_too_large`); nothing is admitted.
- [negative-1.9 - Partition key too long](producer/single/1.09-negative-partition-key-too-long.md) - resolved partition key over 1024 bytes → `400 Invalid Argument` (`partition_key_too_long`); nothing is admitted.
- [negative-1.13 - Nil event tenant](producer/single/1.13-negative-publish-nil-tenant.md) - an event whose `subject_tenant` is the nil UUID → `400 Invalid Argument` (`InvalidTenant`) before any authorization call; nothing is admitted.
- [negative-1.2 — Mixed-topic batch](producer/batch/1.02-negative-mixed-topic-batch.md) — batch events span different topics → `400 Invalid Argument`.
- [negative-1.3 - Batch too large](producer/batch/1.03-negative-batch-too-large.md) - over 100 events or 1 MiB → `413 Content Too Large`.
- [negative-1.4 — Batch late validation failure](producer/batch/1.04-negative-batch-late-validation-failure.md) — later invalid event rejects whole batch → `422 Invalid Argument`.
- [negative-1.5 - Empty batch](producer/batch/1.05-negative-batch-empty.md) - `"events": []` → `400 Invalid Argument` (`empty_batch`).
- [negative-1.6 - Duplicate event id in batch](producer/batch/1.06-negative-batch-duplicate-event-id.md) - an `id` repeated within the batch → `400 Invalid Argument` (`duplicate_event_id`, `events[1].id`); nothing stored.
- [negative-1.7 - Malformed run in batch](producer/batch/1.07-negative-batch-malformed-run.md) - chained run `[0->1, 2->3]` → `400 Invalid Argument` (`malformed_run`, `events[1].meta`); nothing stored, no head row.
- [negative-1.8 - Batch sequence violation](producer/batch/1.08-negative-batch-sequence-violation.md) - a run behind the committed head → `412 Failed Precondition` naming `index`, `producer`, `topic`, `last_sequence`; nothing stored.
- [negative-1.11 - Batch with an event too large](producer/batch/1.11-negative-batch-event-too-large.md) - one event over 60 KiB → `413 Content Too Large` (`event_too_large`, `events[1]`); nothing stored.
- [negative-1.12 - Batch with a partition key too long](producer/batch/1.12-negative-batch-partition-key-too-long.md) - one partition key over 1024 bytes → `400 Invalid Argument` (`partition_key_too_long`, `events[1].partition_key`); nothing stored.
- [negative-1.13 - Batch with an event in an unreachable tenant](producer/batch/1.13-negative-batch-tenant-not-authorized.md) - one event for a tenant the caller cannot reach → `403 Permission Denied` (`TenantIdNotAuthorized`); nothing stored, including the events around it.
- [negative-1.14 - Batch mode-shape violation](producer/batch/1.14-negative-batch-mode-shape-violation.md) - one chained event without `meta.previous` → `400 ChainModeFieldsMissing` for the whole batch; nothing is admitted.
- [negative-1.12 - Monotonic publish carrying previous](producer/flows/1.12-negative-monotonic-publish-carries-previous.md) - monotonic producer sends `meta.previous` → `400 MonotonicModeFieldsViolation`.
- [negative-1.13 - Chained publish without previous](producer/flows/1.13-negative-chained-publish-missing-previous.md) - chained producer omits `meta.previous` → `400 ChainModeFieldsMissing`.
- [negative-1.14 - Chained publish without sequence](producer/flows/1.14-negative-chained-publish-missing-sequence.md) - chained producer omits `meta.sequence` → `400 ChainModeFieldsMissing`.
- [negative-1.15 - Monotonic publish without sequence](producer/flows/1.15-negative-monotonic-publish-missing-sequence.md) - monotonic producer omits `meta.sequence` → `400 MonotonicModeFieldsViolation`.
- [negative-1.16 - Meta without producer](producer/flows/1.16-negative-meta-without-producer.md) - `meta.previous` or `meta.sequence` without `meta.producer` → `400 MetaWithoutProducerId`.
- [negative-1.17 - Unknown meta version](producer/flows/1.17-negative-unknown-meta-version.md) - `meta.version` above `1` → `400 UnknownMetaVersion`.
- [negative-1.7 - Too many interests](consumer/subscriptions/1.07-negative-join-too-many-interests.md) - more than 64 interests in one JOIN → `400 Invalid Argument` (`TooManyInterests`); no subscription is created.
- [negative-1.15 - JOIN sending max_depth](consumer/subscriptions/1.15-negative-join-max-depth-rejected.md) - an interest that sends `max_depth` → `400 Invalid Argument` (`InvalidBody`) naming `ownership_filter`; no subscription created.
- [negative-1.19 - JOIN only_mine for another tenant](consumer/subscriptions/1.19-negative-join-only-mine-foreign-tenant.md) - an `only_mine` interest naming a `subject_tenant` other than the caller's → `400 Invalid Argument` (`InvalidTenant`) before any authorization call; no subscription created.
- [negative-1.20 - Nil interest tenant](consumer/subscriptions/1.20-negative-join-nil-interest-tenant.md) - an interest whose `subject_tenant` is the nil UUID → `400 Invalid Argument` (`InvalidTenant`) before any authorization call; no subscription is created.
- [negative-1.21 - Too many types](consumer/subscriptions/1.21-negative-join-too-many-types.md) - more than 32 `types` in one interest → `400 Invalid Argument` (`TooManyTypes`) before any authorization call; no subscription is created.
- [negative-1.22 - Too many subject types](consumer/subscriptions/1.22-negative-join-too-many-subject-types.md) - more than 32 `subject_types` in one interest → `400 Invalid Argument` (`TooManySubjectTypes`) before any authorization call; no subscription is created.
- [negative-1.6 - Invalid client_agent](consumer/groups/1.06-negative-invalid-client-agent.md) - non-ASCII, oversized or off-grammar `client_agent` → `400 Invalid Argument`.
- [guardrail-1.7 — Stream requires multipart Accept](consumer/stream/1.07-guardrail-stream-accept-json-rejected.md) — `Accept: application/json` on `:stream` endpoint → `406 Invalid Argument`.
- [guardrail-1.8 — SSE from multipart endpoint](consumer/stream/1.08-guardrail-sse-from-stream-endpoint.md) — `Accept: text/event-stream` on `/events:stream` → `406`; use `/events:sse` instead.
- [negative-1.10 — Stream rejects timeout/collect params](consumer/stream/1.10-negative-stream-rejects-timeout-collect-params.md) — unsupported query params on `:stream` → `400 Invalid Argument`.

### Seek / cursor errors

- [negative-1.5 — Out-of-range offset](consumer/positions/1.05-negative-out-of-range-offset.md) — offset below RF−1 → `400 Invalid Argument`.
- [negative-1.6 — Offset above HWM](consumer/positions/1.06-negative-offset-above-hwm.md) — offset beyond HWM → `400 Invalid Argument`.
- [negative-1.7 - SEEK while streaming](consumer/positions/1.07-negative-seek-while-streaming.md) - any SEEK while `:stream` is open → `409 StreamingInProgress` for a caller the identity, tenant, joiner and group checks admit (SEEK is pre-stream-only).
- [negative-1.8 — SEEK unknown subscription](consumer/positions/1.08-negative-seek-unknown-subscription.md) — `:seek` on a non-existent `subscription_id` → `404 Not Found`.
- [negative-1.9 — SEEK unassigned partition](consumer/positions/1.09-negative-seek-unassigned-partition.md) — SEEK references a partition not in `assigned[]` → `409 Failed Precondition`.
- [negative-1.15 - Stale topology version](consumer/positions/1.15-negative-seek-stale-topology-version.md) - `topology_version` differs from the subscription's → `412 Failed Precondition` (`topology_version_mismatch`); nothing is seeded.
- [negative-1.18 - SEEK by another principal](consumer/positions/1.18-negative-seek-subscription-not-owned.md) - a principal that did not JOIN the subscription, even one of its tenant, SEEKs it → `403 Permission Denied` (`SubscriptionNotOwned`) before the open-stream check; no cursor is written.

### Stream errors

- [negative-1.4 — PositionsNotSet](consumer/stream/1.04-negative-stream-positions-not-set.md) — stream opened without prior SEEK → `409 Failed Precondition`; one `positions_not_set` violation per affected partition.
- [negative-1.5 - Unknown subscription](consumer/stream/1.05-negative-stream-unknown-subscription.md) - subscription `{id}` not found or expired → `404 Not Found`.
- [negative-1.6 - Terminated subscription](consumer/stream/1.06-negative-stream-terminated-subscription.md) - delivery shard shutdown (not yet implemented, tracked in #4438) sends `410` (not yet answered, tracked in #5977; a reaped or left member answers `404 SubscriptionNotFound`); consumer re-JOINs.
- [negative-1.11 - Second stream rejected](consumer/stream/1.11-negative-streaming-in-progress.md) - second `:stream` open while one is already active → `409 StreamingInProgress` for a caller the identity, tenant, joiner, group and positions checks admit; existing stream unaffected.
- [negative-1.20 - Stream by another principal](consumer/stream/1.20-negative-stream-subscription-not-owned.md) - a principal that did not JOIN the subscription, even one of its tenant, opens `:stream` → `403 Permission Denied` (`SubscriptionNotOwned`) after the tenant check, before the positions check and the lease; no stream opens.

### Producer chain errors

- [negative-1.5 - Chained sequence violation](producer/flows/1.05-negative-chained-sequence-violation.md) - a chained publish that neither advances the chain nor resends its head → `412 Failed Precondition`; recover via `GET /v1/producers/{id}/cursors`.
- [negative-1.8 - Unknown producer](producer/flows/1.08-negative-unknown-producer.md) - `meta.producer` not registered or reaped (not yet implemented, tracked in #5651) → `404 Not Found` naming the producer.

### Consumer group errors

- [negative-1.5 — Delete group with active members](consumer/groups/1.05-negative-delete-group-with-active-members.md) — `DELETE` while subscriptions exist → `409 Failed Precondition`.
- [negative-1.7 — Get unknown group](consumer/groups/1.07-negative-get-unknown-group.md) — `GET /v1/consumer-groups/{id}` for non-existent id → `404 Not Found`.
- [negative-1.8 — LEAVE unknown subscription](consumer/subscriptions/1.08-negative-leave-unknown-subscription.md) — `DELETE /v1/subscriptions/{id}` for expired/unknown id → `404 Not Found`.
- [negative-1.23 - LEAVE by another principal](consumer/subscriptions/1.23-negative-leave-subscription-not-owned.md) - a principal that did not JOIN the subscription, even one of its tenant, sends `DELETE /v1/subscriptions/{id}` → `403 Permission Denied` (`SubscriptionNotOwned`); the subscription stays.
- [negative-1.9 - Anonymous group access from another tenant](consumer/groups/1.09-negative-cross-tenant-anonymous-group-ownership.md) - the group belongs to its creating principal in its tenant; a principal of a tenant the policy does not let reach the group's tenant: GET and DELETE → `403 Permission Denied` (`TenantIdNotAuthorized`), LIST leaves the group out, JOIN → `403 Permission Denied` (`ConsumerGroupNotOwned`) before any policy call.
- [negative-1.10 - DELETE by another principal](consumer/groups/1.10-negative-delete-anonymous-group-not-owned.md) - a principal other than the anonymous group's creator in its tenant (another subject of that tenant, or the creator's subject in another tenant) sends `DELETE /v1/consumer-groups/{id}` → `403 Permission Denied` (`ConsumerGroupNotOwned`) after the tenant check, before the active-members check; the group stays.
- [negative-1.16 - JOIN by another principal](consumer/subscriptions/1.16-negative-join-anonymous-group-not-owned.md) - a principal other than the anonymous group's creator in its tenant (another subject of that tenant, or the creator's subject in another tenant) JOINs it → `403 Permission Denied` (`ConsumerGroupNotOwned`) before any policy call; no subscription is created and the group's members stay.
- [negative-1.17 - JOIN, stream or SEEK without consume on the group](consumer/subscriptions/1.17-negative-join-consumer-group-not-authorized.md) - a principal the policy refuses `consume` on the consumer group JOINs it, or opens `:stream` or SEEKs once that grant is revoked → `403 Permission Denied` (`ConsumerGroupNotAuthorized`); no subscription, lease or cursor (only an action-aware policy refuses it).
- [negative-1.13 — JOIN group at capacity](consumer/subscriptions/1.13-negative-join-group-at-capacity.md) — all partitions already assigned; new JOIN → `429` with `Retry-After`; zero-partition standby not admitted.

### Topics / segments errors

- [negative-1.3 — Segments for unknown topic](topics/1.03-negative-segments-unknown-topic.md) — `GET /v1/topics/segments` with unregistered topic → `404 Not Found`.

### Rate limiting

- [negative-1.4 — Publish rate limited](producer/single/1.04-negative-rate-limited.md) — publish exceeds per-tenant quota → `429 Resource Exhausted` with `Retry-After`.

### Error envelope reference

- [1.01 — Problem Details envelope](errors/1.01-positive-problem-details-envelope.md) — canonical RFC-9457 + GTS shape; all broker errors use this format.
- [1.02 — 401 Unauthenticated](errors/1.02-negative-401-unauthenticated.md)
- [1.03 — 403 Permission Denied](errors/1.03-negative-403-unauthorized.md)
- [1.04 — 404 Not Found](errors/1.04-negative-404-not-found.md)
- [1.05 — 409 Failed Precondition](errors/1.05-negative-409-conflict.md)
- [1.06 — 412 Failed Precondition (sequence)](errors/1.06-negative-412-sequence-violation.md)
- [1.07 — 429 Resource Exhausted](errors/1.07-negative-429-rate-limited.md)
- [1.08 — 500 Internal](errors/1.08-negative-500-internal.md)

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
[Create anonymous group](../groups/1.01-positive-create-anonymous-group.md)
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
| 404, 410 (410 not yet answered, tracked in #5977) | `not_found` | `"Not Found"` |
| 409, 412 | `failed_precondition` | `"Failed Precondition"` |
| 429 | `resource_exhausted` | `"Resource Exhausted"` |
| 500 | `internal` | `"Internal"` |

Domain-specific detail goes inside `context`, not at root level, and only in the keys `toolkit-canonical-errors` can actually produce. Its builder composes `context` from a closed set - field violations, precondition violations, quota violations, a format message, a constraint message, a reason - plus `resource_type` / `resource_name` from the resource error. The entries themselves are fixed shapes: a field violation is `{field, description, reason}`, a precondition violation is `{type, subject, description}`.

So a scenario must not invent a `context` key. Anything a caller needs that has no key of its own belongs inside a `description`: the broker's `last_sequence` on a sequence violation, or the valid range on an out-of-range seek, are written into the violation's `description` text, not added as fields. The wire type is free-form JSON, so an invented key is not *impossible* - it would mean bypassing the canonical builder, which is exactly what the gear must not do.

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
| `RF` | Backend partition retention floor — smallest offset still readable. |
| `HWM` | Backend partition high-water mark — offset of the next event to be admitted. |
| `MP` | Multipart frame on `:stream`. |
| `SSE` | Server-Sent Event frame on `:sse`. |
| `Cursor` | The stored position for a `(group, topic, partition)` that the last SEEK set; the broker emits from `Cursor + 1`. Delivery does not move it. |
