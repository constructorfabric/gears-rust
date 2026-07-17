<!-- Created: 2026-05-11 by Constructor Tech -->

# Feature: Consumer Subscription Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-evbk-featstatus-consumer-subscription-lifecycle`

<!-- toc -->

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Actor Flows (CDSL)](#2-actor-flows-cdsl)
  - [2.1 Cold JOIN (new consumer, fresh group)](#21-cold-join-new-consumer-fresh-group)
  - [2.2 SEEK (set cursor; pre-stream or forward advance)](#22-seek-set-cursor-pre-stream-or-forward-advance)
  - [2.3 Re-JOIN after 410 Gone / 404 / shard failover](#23-re-join-after-410-gone--404--shard-failover)
  - [2.4 Upscaling (add consumer instance)](#24-upscaling-add-consumer-instance)
  - [2.5 Downscaling (remove consumer instance)](#25-downscaling-remove-consumer-instance)
  - [2.6 Filter / topic-list mutation during rolling deploy](#26-filter--topic-list-mutation-during-rolling-deploy)
  - [2.7 Session timeout (heartbeat-via-poll)](#27-session-timeout-heartbeat-via-poll)
  - [2.8 Delivery shard ownership change](#28-delivery-shard-ownership-change)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
- [4. States (CDSL)](#4-states-cdsl)
- [5. Definitions of Done](#5-definitions-of-done)
  - [Broker-Side Implementation](#broker-side-implementation)
- [6. Acceptance Criteria](#6-acceptance-criteria)
- [7. Unit Test Plan](#7-unit-test-plan)
- [8. E2E Test Plan](#8-e2e-test-plan)

<!-- /toc -->

## 1. Feature Context

### 1.1 Overview

Consumers interact with the Event Broker through a JOIN → poll → seek → leave cycle bound to a consumer group. This feature catalogues the eight named flows that compose the full lifecycle, each described in CDSL with explicit broker-side state transitions and consumer-visible error / recovery paths. The flows cover both happy and unhappy paths and span both standalone (in-process cache) and cluster (persistent cache provider) deployment modes.

### 1.2 Purpose

Consumer subscription lifecycle defines the end-to-end contract a consumer SDK must implement to join a consumer group, poll events, advance the cursor via SEEK, handle rebalance / session-loss / shard-failover events, and leave cleanly. Without a uniform contract, consumer fleets drift across deployments — one absorbs `410 Gone` cleanly while another spins on it; one survives a rolling upscale with cursor continuity while another duplicates batches; one re-joins gracefully after a shard reassignment while another loses in-flight events. The feature pins down eight scenarios spanning the full lifecycle (cold JOIN, offset seek, re-JOIN after `410`/`404`/failover, upscale, downscale, filter mutation mid-rollout, session-timeout, shard ownership change), each with explicit state transitions, error surfaces, and recovery paths, so any conformant consumer behaves predictably on both standalone (in-process cache) and cluster (persistent provider) deployments.

### 1.3 Actors

- **Consumer SDK** (`cf-gears-event-broker-sdk`): JOINs, polls, seeks, leaves.
- **DispatcherService**: routes consumer-group requests to the owning delivery shard.
- **DeliveryService**: per-shard owner of `GroupState`; runs the rebalance algorithm; serves polls.
- **ClusterCapabilities** (cluster mode): provides distributed locks, leader election, persistent cache for cursors / GroupState.

### 1.4 References

- DESIGN.md §3.1 Subscription + GroupState + Cursor entities
- DESIGN.md §3.2 Subscription Lifecycle + Long-Poll Mechanism
- DESIGN.md §3.5 Long-Poll Consumption Flow
- DESIGN.md §3.6 (delivery shard ownership and SD double-check)
- USE_CASES.md §1 Consumer State Machine, §4 Walked Scenarios, §5 Rebalance Algorithm
- `openapi.yaml` — wire shapes for all endpoints referenced below
- `ADR/0002-consumption-transport.md` — the transport (long-poll vs SSE vs multipart) is open; flows below assume the current long-poll transport.

## 2. Actor Flows (CDSL)

Pseudo-code below is Python-ish — not runnable; intent over syntax. Each flow shows producer/consumer/broker steps at API granularity.

### 2.1 Cold JOIN (new consumer, fresh group)

```python
# Step 0 — anonymous case: caller creates the group; named case skips this
resp = http.post("/v1/consumer-groups", json={
    "client_agent": "myservice/1.0.0 myeventlib/2.0.0",
})
assert resp.status == 201
group_id     = resp.json["id"]              # e.g. gts.cf.core.events.consumer_group.v1~<uuid>
client_agent = resp.json["client_agent"]    # echoed back; persisted on the group row
# named groups: group_id is provisioned via types_registry at broker startup; skip POST.
# client_agent is required (RFC 9110 User-Agent grammar; ASCII; 1–256 bytes); informational
# only — does not participate in dedup / authz / ownership decisions. Immutable after create.
# Validation failure surfaces as the canonical RFC 9457 400 problem type.

# Step 1 — JOIN with topic-anchored typed-filter interests (per ADR-0005)
resp = http.post("/v1/subscriptions", json={
    "consumer_group":   group_id,
    "client_agent":     "myservice/1.0.0 myeventlib/2.0.0",
    "session_timeout":  "PT30S",
    "interests": [
        {
            "topic":           "gts.cf.core.events.topic.v1~yourorg.orders.v1",
            "tenant_id":       tenant_id,
            "types":           ["gts.cf.core.events.event.v1~yourorg.orders.*"],
            # Optional paired filter — omit both for "no expression filter":
            "expression_type": "gts.cf.core.events.filter.v1~cf.core.expression.cel.v1",
            "expression":      "event.data.amount > 100",
        },
    ],
})
sub_id              = resp.json["id"]
assigned            = resp.json["assigned"]            # list of (topic, partition); topic-centric for seek
topology_version    = resp.json["topology_version"]

# Broker-side join logic
def join(req):
    shard = dispatch_route(req.consumer_group)         # via cluster lookup or claim
    with cluster.distributed_lock(f"evbk.group.{req.consumer_group}"):
        gs = GroupState.get_or_create(req.consumer_group)
        gs.active_members[sub_id] = Member(req.topics, req.filters)
        gs.topology_version += 1
        rebalance(gs)                                  # round-robin v1, USE_CASES.md §5
    return Subscription(id=sub_id, assigned=gs.active_members[sub_id].assigned,
                        topology_version=gs.topology_version)

# Step 2 — pre-stream SEEK (required; OffsetManager-driven)
# The SDK resolves a starting position per assigned (topic, partition) via
# OffsetManager.position(...). For a fresh group the manager returns the
# configured Fallback sentinel; for a re-running consumer with committed
# cursors it returns Exact(last_processed_offset).
positions = {}
for (topic, partition) in assigned:
    pos = offset_manager.position(group_id, topic, partition)   # ResolvedPosition
    positions[f"{topic}:{partition}"] = pos.to_wire()           # int | "earliest" | "latest"

resp = http.post(f"/v1/subscriptions/{sub_id}:seek", json={
    "partition_positions": positions,
})
assert resp.status == 200    # 400 InvalidInitialPosition if an integer is out of range

# Step 3 — first stream
for frame in http.get(f"/v1/events:stream?subscription_id={sub_id}", stream=True):
    handle(frame)
# Defensive: broker returns 409 PositionsNotSet if any assigned partition lacks
# a cursor. A well-behaved SDK never sees this on the happy path because Step 2
# seeded every partition. SDK recovery: re-resolve via position() + re-SEEK.

# Broker serves from cursor + 1 (where `cursor` is the session/runtime
# cursor for the subscription's assigned partition). Sentinels resolve at admission:
#   "earliest" → cursor := retention_floor - 1 (emit from retention_floor)
#   "latest"   → cursor := current high-water mark (emit only future events)

# Durability matrix:
#   standalone           → cursor dies with the process
#   cluster + persistent → cursor survives shard restart (Redis-with-disk, etcd)
```

### 2.2 SEEK (set cursor; pre-stream or forward advance)

The `POST /v1/subscriptions/{id}:seek` endpoint serves two roles for the same subscription:

1. **Pre-stream seed** (§2.1 Step 2) — call it once after JOIN to declare the starting position for each assigned partition. Any value in the valid range `[retention_floor - 1, high_water_mark]` is permitted; sentinels `"earliest"` / `"latest"` are server-resolved at admission.
2. **Forward SEEK during streaming** — call it to advance the cursor past processed events. While `:stream` is open against the subscription, the broker enforces `MAX(stored, requested)` per partition; backward moves are rejected with `409 SeekBackwardNotAllowed`.

```python
# Consumer side — pre-stream seed (per-partition; values are int OR "earliest"/"latest")
resp = http.post(f"/v1/subscriptions/{sub_id}:seek", json={
    "partition_positions": {
        "orders:0": 42,         # last-processed offset; broker emits from 43
        "orders:1": "earliest", # broker sets cursor := retention_floor - 1
        "orders:2": "latest",   # broker sets cursor := current high-water mark
    }
})
assert resp.status == 200

# Consumer side — forward SEEK during streaming (integers only; forward-only enforced)
resp = http.post(f"/v1/subscriptions/{sub_id}:seek", json={
    "partition_positions": {"orders:0": 100}  # 409 SeekBackwardNotAllowed if < 42
})

# Broker side
def seek(sub_id, partition_positions, stream_is_open):
    sub = state.subscriptions[sub_id]
    for (key, value) in partition_positions.items():
        topic, part = key.split(":")
        if (topic, int(part)) not in sub.assigned:
            return 409_PartitionNotAssigned                  # atomic: nothing applies

        resolved = resolve(value, topic, int(part))          # int verbatim or sentinel→int
        if not (retention_floor(topic, int(part)) - 1 <= resolved <= high_water_mark(topic, int(part))):
            return 400_InvalidInitialPosition

        if stream_is_open and resolved < cursor.get(sub.group, topic, int(part)):
            return 409_SeekBackwardNotAllowed                 # forward-only during streaming

        cursor.set((sub.group, topic, int(part)), resolved)
    return 200_OK
```

Sentinel resolution (broker-side, at admission):
- `"earliest"` → cursor set to `retention_floor - 1` for `(group, topic, partition)`. Subsequent emission begins at `retention_floor`.
- `"latest"` → cursor set to the current high-water mark for `(group, topic, partition)`. Subsequent emission includes only events admitted after the SEEK lands.

### 2.3 Re-JOIN after 410 Gone / 404 / shard failover

```python
# Consumer SDK loop
while True:
    try:
        for frame in http.get(f"/v1/events:stream?subscription_id={sub_id}", stream=True):
            handle(frame)
            if frame.kind == "topology":
                # Re-SEEK newly-assigned partitions (continuing partitions keep their cursor).
                new_slots = [s for s in frame.assigned if s not in prev_assigned]
                if new_slots:
                    positions = {f"{t}:{p}": offset_manager.position(group_id, t, p).to_wire()
                                 for (t, p) in new_slots}
                    http.post(f"/v1/subscriptions/{sub_id}:seek",
                              json={"partition_positions": positions})
                prev_assigned = frame.assigned
    except (HTTP_410_Gone, HTTP_404_SubscriptionNotFound):
        drop_in_flight(sub_id)                         # any unfinished work for this sub
        sub_id, assigned = re_join(group_id, interests)
        # Fresh subscription_id → no SEEK history on broker → must re-seed
        positions = {f"{t}:{p}": offset_manager.position(group_id, t, p).to_wire()
                     for (t, p) in assigned}
        http.post(f"/v1/subscriptions/{sub_id}:seek",
                  json={"partition_positions": positions})
        continue
    except TransportError:                             # ungraceful shard kill
        sub_id, assigned = re_join(group_id, interests)
        positions = {f"{t}:{p}": offset_manager.position(group_id, t, p).to_wire()
                     for (t, p) in assigned}
        http.post(f"/v1/subscriptions/{sub_id}:seek",
                  json={"partition_positions": positions})
        continue

# Cursor durability on re-JOIN:
#   standalone           → cursor lost; resume from earliest_available_offset
#   cluster + persistent → cursor preserved; resume from cursor.offset + 1
# Consumers MUST handle at-least-once: dedup by event.id on the consumer side.
```

### 2.4 Upscaling (add consumer instance)

```python
# New member joins
sub_id_new = http.post("/v1/subscriptions", json={"consumer_group": G, "client_agent": CA, "interests": [...]}).json["id"]

# Broker side
def join_new_member(G, member):
    with cluster.distributed_lock(f"evbk.group.{G}"):
        gs = GroupState[G]
        gs.active_members[member.id] = member
        gs.topology_version += 1
        rebalance(gs)                                  # round-robin v1
        for existing_member in gs.active_members.values():
            if existing_member.id != member.id:
                cluster.publish(f"evbk.poller_wake:{existing_member.id}")
    # All in-flight pollers wake early; their next response carries the new
    # topology_version + updated assigned set.

# INVARIANT: no (topic, partition) is processed by two consumers simultaneously.
# The distributed_lock around GroupState mutation serializes the reassignment.
```

### 2.5 Downscaling (remove consumer instance)

```python
# Graceful leave
http.delete(f"/v1/subscriptions/{sub_id}")             # explicit LEAVE
# OR — silent death: consumer just stops polling

# Broker side
def remove_member(G, member_id, reason):               # reason in {"leave", "session_timeout"}
    with cluster.distributed_lock(f"evbk.group.{G}"):
        gs = GroupState[G]
        del gs.active_members[member_id]
        gs.topology_version += 1
        rebalance(gs)                                  # redistribute departed member's partitions
        for survivor in gs.active_members.values():
            cluster.publish(f"evbk.poller_wake:{survivor.id}")
    # In-flight events held in the departed member's buffer are NOT shipped
    # elsewhere — the new owner re-reads from cursor.offset + 1 (single source of truth).

# Reaper fires the silent-death path:
async def reaper_loop():
    while True:
        await sleep(5)
        for sub in state.subscriptions.values():
            if sub.expires_at < now():
                remove_member(sub.group, sub.id, reason="session_timeout")
```

### 2.6 Filter / topic-list mutation during rolling deploy

```python
# During the rolling deploy, both v1 (filters=F1, topics=T1) and v2 (F2, T2)
# members coexist in group G. Each member declares its own filters and topic
# list at JOIN; the rebalance assigns each partition to exactly one member.

def deliver_to_member(member, raw_events):
    matching = [e for e in raw_events if member.filters.match(e)]
    return matching

# Cursor handoff at partition reassignment:
#   v1 member owns (T, p) with cursor.offset = X
#   rebalance reassigns (T, p) to a v2 member
#   v2 member resumes from cursor.offset + 1 = X + 1
#   ← cursor reflects the v1 member's processing position at the handoff
#   (R60 in DESIGN.md: accepted rollout behavior)

# When all v1 members leave, the group settles into the F2/T2 worldview.
```

### 2.7 Session timeout (heartbeat-via-poll)

```python
# Each poll arrival refreshes the subscription's expires_at
def on_poll_arrival(sub_id):
    sub = state.subscriptions[sub_id]
    sub.expires_at = now() + sub.session_timeout       # heartbeat-via-poll

# Reaper (see §2.5) deletes subs past expires_at and triggers rebalance.
# Consumer's next poll after the reap returns 404 SubscriptionNotFound;
# consumer re-JOINs per §2.3.

# IMPORTANT: session_timeout MUST exceed the chosen poll timeout to avoid
# reap-during-poll. Recommended:
#   poll_timeout      = 20  # seconds
#   session_timeout   = 60  # seconds
```

### 2.8 Delivery shard ownership change

```python
# Graceful release (e.g., shard A scaling down)
async def release_ownership(G):
    for poller in pollers_of_group(G):
        respond_410_gone(poller)                       # consumer will re-JOIN
    await cluster.distributed_lock.release(f"evbk.group.{G}")
    # Next shard B to acquire the lock claims ownership and runs a fresh rebalance.

# Ungraceful kill (shard A forcibly terminated)
#   - In-flight pollers see TCP RST or read timeout
#   - cluster.distributed_lock has a session TTL; the lock auto-releases on TTL expiry
#   - Shard B acquires the lock; reconstructs GroupState from cache (cluster) or
#     starts fresh (standalone or non-persistent cache)
#   - Existing sub_ids in cache are honored if session_timeout hasn't elapsed;
#     otherwise treated as gone (consumer re-JOINs)

# Cursor durability across ownership change:
#   standalone           → no shard concept; broker restart = full cache wipe
#   cluster + persistent → cursor.offset survives via Redis-with-disk / etcd
```

## 3. Processes / Business Logic (CDSL)

The lock around `cluster.distributed_lock("evbk.group.<G>")` is the serialization point for every GroupState mutation (JOIN, LEAVE, rebalance, ownership transfer). Stream reads and pre-stream SEEK are NOT lock-protected by the GroupState mutation lock — they touch runtime cursor cache keys directly and rely on the cache's own consistency guarantees.

`topology_version` is monotonically increasing per group; every mutation increments it. Consumers compare across poll responses to detect rebalance.

## 4. States (CDSL)

| State | Trigger | Next |
|---|---|---|
| Joining | POST /v1/subscriptions accepted | Active |
| Active | rebalance assigned partitions | Streaming / Seeking |
| Streaming | GET /v1/events:stream in flight | Active (response closes) / Reaped (session_timeout or disconnect) |
| Reaped | session_timeout elapsed with no active stream | Gone |
| Gone | LEAVE or Reap | terminal — must re-JOIN to recover |

## 5. Definitions of Done

### Broker-Side Implementation

- [ ] `p1` - **ID**: `cpt-cf-evbk-dod-consumer-subscription-lifecycle-broker`

- DispatcherService routes by `consumer_group` GTS id to the owning shard.
- DeliveryService implements the v1 rebalance algorithm under `cluster.distributed_lock`.
- `GroupState` carries per-member filters / topic lists; rebalance respects them.
- `cursor.offset` is updated atomically via cache CAS (compare-and-swap with `>=` semantics — forward-only during streaming; the SEEK endpoint accepts any valid range pre-stream).
- `topology_version` is exposed on every poll response.
- Reaper removes subscriptions past `expires_at` and triggers rebalance.
- The eight flows above are implemented end-to-end and visible through `evbk_*` metrics.

## 6. Acceptance Criteria

- **AC-1 (cold JOIN)**: POST /v1/consumer-groups with `{ "client_agent": "<valid User-Agent string>" }` returns 201 + Location + broker-minted id + echoed `client_agent`; subsequent POST /v1/subscriptions referencing that id succeeds. POST without `client_agent` is rejected with the canonical RFC 9457 400 problem type.
- **AC-2 (seek)**: SEEK (`POST /v1/subscriptions/{id}:seek`) on assigned partitions updates `cursor.offset`; next poll returns events from the new offset; SEEK on unassigned partition returns 409.
- **AC-3 (re-JOIN)**: a poller receiving 410 Gone successfully re-JOINs with a fresh subscription id and resumes consumption.
- **AC-4 (upscale)**: adding member N+1 causes existing members to receive a new `topology_version` and updated `assigned` set within 5s.
- **AC-5 (downscale)**: graceful LEAVE redistributes partitions within 5s; silent death takes effect within `session_timeout`.
- **AC-6 (filter mutation)**: a partition handoff from v1 to v2 member preserves the cursor; no events are double-delivered to v1 after handoff.
- **AC-7 (session timeout)**: a consumer that stops polling for `session_timeout + 1s` finds its subscription reaped on next attempt; re-JOIN succeeds.
- **AC-8 (shard ownership change)**: graceful shutdown of owning shard delivers 410 to all in-flight pollers within 1s; ungraceful kill results in transport error within the cluster lock's session TTL.
- **AC-9 (typed filter happy path)**: JOIN with an interest carrying `expression_type: "gts.cf.core.events.filter.v1~cf.core.expression.cel.v1"` + `expression: "event.data.amount > 100"` succeeds; subsequent polls deliver only events matching the predicate; non-matching events are silently dropped.
- **AC-10 (no filter)**: JOIN with an interest omitting `expression_type` + `expression` succeeds; subsequent polls deliver every event matching topic + tenant + types (no engine invocation).
- **AC-11 (paired-optional violation)**: JOIN with an interest supplying `expression_type` without `expression` (or vice versa) is rejected with `400 BadRequest`.
- **AC-12 (invalid type pattern)**: JOIN with `types: ["gts.cf.core.events.event.v1~yourorg.*.placed.v1"]` (mid-pattern wildcard) is rejected with `400 BadTypePattern`.
- **AC-13 (zero-match type pattern)**: JOIN with a `types[]` pattern matching no registered types under the declared topic is rejected with `400 NoTypesMatched`.
- **AC-14 (type-belongs-to-topic)**: JOIN with a `types[]` pattern that would resolve to a type whose `parent_topic` differs from `interest.topic` is rejected with `400 TypeNotInTopic` (defense-in-depth).
- **AC-15 (rolling-deploy correctness)**: members M1 (interests pointing at `placed.v1`) and M2 (interests pointing at `placed.v2`) coexist in one group; rebalance produces correct per-member assignments; events of `placed.v1` ship only to M1; events of `placed.v2` ship only to M2.
- **AC-16 (filter eval timeout drops, does not fail poll)**: an interest whose `engine.eval` exceeds `EVAL_TIMEOUT_MICROS` produces a warn log + `evbk_filter_eval_timeout_total{consumer_group}` increment; the poll returns `200 OK` with the event dropped from this consumer's batch.

## 7. Unit Test Plan

- **rebalance algorithm**: parameterize over (member counts, topic subsets, partition counts); assert single-consumer-per-partition invariant + per-member topic respect.
- **cursor CAS forward-only**: SEEK with offset older than current `cursor.offset` is a no-op (forward-only during streaming).
- **topology_version monotonicity**: each mutation increments by exactly 1; concurrent mutations serialize through the lock.
- **filter evaluation order**: per-member filter applies AFTER partition assignment; events for non-assigned partitions are never evaluated.
- **interest prerequisites**: per-event delivery checks `event.topic == interest.topic`, then `event.tenant_id == interest.tenant_id`, then `event.type ∈ resolved_type_set` BEFORE invoking `engine.eval`. Non-matching prerequisite → skip interest without engine call.
- **paired-optional shape**: parameterize over (`expression_type`, `expression`) ∈ {(set, set), (set, absent), (absent, set), (absent, absent)} — first and last accepted; middle two rejected with `400 BadRequest`.
- **GTS pattern syntax**: parameterize over valid (exact, trailing `.*`, trailing `~*`, `v*` at trailing) and invalid (mid-pattern `*`, `**`, substring `vendor*`, two `*` occurrences) patterns; valid → accept; invalid → `400 BadTypePattern`.
- **Version resolution**: parameterize over (registered types, pattern) tuples and assert resolved set matches per-name-latest + minor-version-omitted rule from ADR-0005.
- **Filter context (CEL engine)**: assert `event.id` / `event.type` / ... visible; `event.meta` reference → `FilterError::CompileFailed`.

## 8. E2E Test Plan

- **S1 — full lifecycle**: POST /v1/consumer-groups → JOIN → SEEK → poll → SEEK (advance cursor) → poll → LEAVE; verify every state transition.
- **S2 — concurrent JOINs**: 100 members JOIN within 1s; verify all assigned at least one partition; verify no partition is double-assigned.
- **S3 — rolling deploy**: start with 5 v1 members; introduce 5 v2 members one at a time; remove v1 members; assert no event is lost, no event is double-processed.
- **S4 — shard kill**: ungracefully kill owning shard; another shard takes over within the cluster lock TTL; consumers re-JOIN and resume.
- **S5 — proxy idle behavior**: place a 25s-idle-timeout proxy between consumer and broker; verify consumer's 20s poll completes through the proxy.
- **S6 — standalone cursor loss**: in standalone mode, restart broker; subscription gone, cursor gone; new JOIN resumes from `earliest_available_offset`.
- **S7 — cluster cursor durability**: in cluster mode with persistent cache provider, restart owning shard; new shard takes over; cursor preserved; consumer resumes from `cursor.offset + 1` (i.e., SEEK sets `cursor.offset`, broker emits from `cursor.offset + 1`).
- **S8 — session timeout reap**: configure `session_timeout=PT3S`; consumer stops polling; after 5s, attempt poll; verify 404 SubscriptionNotFound; re-JOIN succeeds.
