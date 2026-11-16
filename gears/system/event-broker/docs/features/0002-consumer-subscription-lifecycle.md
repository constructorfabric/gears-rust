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
  - [2.2 SEEK (set cursor; pre-stream)](#22-seek-set-cursor-pre-stream)
  - [2.3 Re-JOIN after 410 Gone / 404 / shard failover](#23-re-join-after-410-gone--404--shard-failover)
  - [2.4 Upscaling (add consumer instance)](#24-upscaling-add-consumer-instance)
  - [2.5 Downscaling (remove consumer instance)](#25-downscaling-remove-consumer-instance)
  - [2.6 Filter / topic-list mutation during rolling deploy](#26-filter--topic-list-mutation-during-rolling-deploy)
  - [2.7 Session timeout](#27-session-timeout)
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

Consumers interact with the Event Broker through a JOIN → seek → stream → leave cycle bound to a consumer group. This feature catalogues the eight named flows that compose the full lifecycle, each described in CDSL with explicit broker-side state transitions and consumer-visible error / recovery paths. The flows cover both happy and unhappy paths and span both standalone (in-process cache) and cluster (persistent cache provider) deployment modes.

### 1.2 Purpose

Consumer subscription lifecycle defines the end-to-end contract a consumer SDK must implement to join a consumer group, stream events, advance the cursor via SEEK, handle rebalance / session-loss / shard-failover events, and leave cleanly. Without a uniform contract, consumer fleets drift across deployments - one absorbs `410 Gone` cleanly while another spins on it; one survives a rolling upscale with cursor continuity while another duplicates batches; one re-joins gracefully after a shard reassignment while another loses in-flight events. The feature pins down eight scenarios spanning the full lifecycle (cold JOIN, offset seek, re-JOIN after `410`/`404`/failover, upscale, downscale, filter mutation mid-rollout, session-timeout, shard ownership change), each with explicit state transitions, error surfaces, and recovery paths, so any conformant consumer behaves predictably on both standalone (in-process cache) and cluster (persistent provider) deployments.

### 1.3 Actors

- **Consumer SDK** (`cf-gears-event-broker-sdk`): JOINs, seeks, opens the stream, leaves.
- **DispatcherService**: routes consumer-group requests to the owning delivery shard (not yet implemented, tracked in #4438).
- **DeliveryService**: per-shard owner of `GroupState` (not yet implemented, tracked in #4438); runs the rebalance algorithm; serves streams.
- **ClusterCapabilities** (cluster mode): provides distributed locks, leader election, persistent cache for cursors / GroupState.

### 1.4 References

- DESIGN.md §3.1 Subscription + GroupState + Cursor entities
- DESIGN.md §3.2, "Subscription Lifecycle"
- DESIGN.md §3.6, "Stream Consumption Flow"
- DESIGN.md §3.2, "Delivery Routing (Cache-Based, Not Hash-Based)" and "Failover" (delivery shard ownership and the SD liveness check)
- `openapi.yaml` — wire shapes for all endpoints referenced below
- [`0004-consumption-transport.md`](0004-consumption-transport.md) - the transport: the subscription stream (`multipart/mixed` on `/v1/events:stream`, SSE on `/v1/events:sse`); the flows below use it.

## 2. Actor Flows (CDSL)

Pseudo-code below is Python-ish — not runnable; intent over syntax. Each flow shows producer/consumer/broker steps at API granularity.

### 2.1 Cold JOIN (new consumer, fresh group)

```python
# Step 0 - anonymous case: caller creates the group; named case skips this (not yet implemented, tracked in #5702)
resp = http.post("/v1/consumer-groups", json={
    "client_agent": "myservice/1.0.0 myeventlib/2.0.0",
})
assert resp.status == 201
group_id     = resp.json["id"]              # e.g. gts.cf.core.events.consumer_group.v1~<uuid>
client_agent = resp.json["client_agent"]    # echoed back; persisted on the group row
# named groups: group_id is provisioned via types_registry at broker startup (not yet
# implemented, tracked in #5702; a named group's JOIN answers 501); skip POST.
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
            "subject_tenant":  tenant_id,           # optional; default the caller's tenant
            "ownership_filter": "entire_tenant",    # optional; default only_mine
            "types":           ["gts.cf.core.events.event.v1~yourorg.orders.*"],
            # Optional subject-type selector (1-32 GTS type patterns): a delivery
            # filter on event.subject_type, not an access grant; omit or [] for none.
            "subject_types":   ["gts.yourorg.orders.customer.v1~"],
            # Optional filter object — omit entirely for "no expression filter":
            "filter": {
                "engine":     "gts.cf.core.events.filter.v1~cf.core.expression.cel.v1",
                "expression": "event.data.amount > 100",
            },
        },
    ],
})
sub_id              = resp.json["id"]
assigned            = resp.json["assigned"]            # list of (topic, partition); topic-centric for seek
topology_version    = resp.json["topology_version"]    # echo back on SEEK (rebalance fence)
created_at          = resp.json["created_at"]          # set at JOIN; list sort key (newest first)
# Per interest, the types[] entries that match no event type registered on its topic
# ([] when all matched). The JOIN still succeeds; such a type is delivered once it
# registers and the caller's consume scope admits it. Only the JOIN response carries it.
unmatched           = [i["unmatched_types"] for i in resp.json["interests"]]

# Broker-side join logic. The REST handler has already parsed the body: 400 for JSON
# that does not parse, 422 for a missing, unknown or mistyped field (a filter without
# engine or expression included), 400 for a bad client_agent, session_timeout or group
# id, then 501 for a named group, then 400 for a bad interest field or pattern. In
# process, the SDK-to-domain conversion parses the group and topic ids. Everything
# below is the domain, and a refused JOIN creates nothing.
def join(ctx, req):
    # runs on the delivery instance the dispatcher picked (feature 0005 §4.3)
    # Over REST the handler has already refused a bad client_agent or session_timeout;
    # in process they are refused here.
    if not valid_client_agent(req.client_agent):       # 1-256 printable ASCII, RFC 9110 grammar
        return 400                                     # ascii_only, field_too_long or user_agent_grammar
    if req.session_timeout <= 0:                       # in process; REST refused it first
        return 400_InvalidSessionTimeout
    if ctx.subject_id.is_nil() or ctx.tenant_id.is_nil():
        return 403_TenantIdNotAuthorized               # before any read or PDP call; a token does not exempt it
    if len(req.interests) > 64:
        return 400_TooManyInterests
    for i in req.interests:                            # in order, per interest
        if len(i.types) > 32:          return 400_TooManyTypes
        if len(i.subject_types) > 32:  return 400_TooManySubjectTypes
        if i.subject_tenant.is_nil():  return 400_InvalidTenant
        if i.ownership_filter == "only_mine" and i.subject_tenant != ctx.tenant_id:
            return 400_InvalidTenant                   # only_mine reads the caller's own tenant
    group = groups.find(req.consumer_group)            # first read
    if group is None:
        return 404_ConsumerGroupNotFound
    if group.anonymous and not group.created_by(ctx):  # its subject, in its tenant
        return 403_ConsumerGroupNotOwned               # compared before any PDP call
    if not pdp.group_check(ctx, group, "consume"):     # consume on the group; an allow is kept in the memo
        return 403_ConsumerGroupNotAuthorized          # 500 when its answer does not compile
    if any(not event_family_overlaps(p) for i in req.interests for p in i.types):
        return 400_BadTypePattern                      # a pattern that can match no event type
    decided = []
    for g in interest_groups(req.interests):           # same (topic, tenant context)
        candidates = spec_cache.event_types_on(g.topic)                    # 503 when unavailable
        if g.barrier_mode == "ignore" and not pdp.barrier_right(ctx, g):   # consume_ignore_barrier
            return 403_TenantIdNotAuthorized           # denied, or an answer that does not compile
        scope = pdp.consume_scope(ctx, g, candidates)  # an allowed answer is kept in the memo
        if scope.refused:                              # denied, not compilable, or nothing enforceable
            return 403_TopicNotAuthorized if pdp.tenant_check(ctx, g.tenant_context) \
                   else 403_TenantIdNotAuthorized      # the tenant check only picks the code
        decided.append((g, scope))                     # the first refused group ends the JOIN
    for (g, scope) in decided:
        g.check_exact_selectors(scope)                 # 403 EventTypeNotAuthorized / SubjectTypeNotAuthorized
    for i in req.interests:
        if not spec_cache.topic(i.topic):
            return 404_TopicNotFound
    repo.mark_subscription(sub_id, req.consumer_group) # routing markers before the member;
    repo.mark_group(req.consumer_group)                # 503 when the cluster cache is unavailable
    with cluster.distributed_lock(f"evbk.group.{req.consumer_group}"):
        # the cluster lock is not yet implemented, tracked in #5979
        # this instance's coordinator lock serializes it today
        gs = GroupState[req.consumer_group]
        gs.evict_disconnected_members()                # a JOIN fills their gap
        gs.members[sub_id] = Member(req.interests)
        rebalance(gs)                                  # range assignment, §3; bumps topology_version
    return Subscription(id=sub_id, assigned=gs.members[sub_id].assigned,
                        topology_version=gs.topology_version)

# Step 2 — pre-stream SEEK (required; OffsetStore-driven)
# The SDK resolves a starting position per assigned (topic, partition) via
# OffsetStore.load_position(...). For a fresh group the store returns the
# configured Fallback sentinel; for a re-running consumer with committed
# cursors it returns Exact(last_processed_offset).
# The wire groups positions by topic: {topic: [{partition, value}]}.
positions = {}
for (topic, partition) in assigned:
    pos = offset_manager.position(group_id, topic, partition)   # ResolvedPosition
    positions.setdefault(topic, []).append(
        {"partition": partition, "value": pos.to_wire()})       # int | "earliest" | "latest"

resp = http.post(f"/v1/subscriptions/{sub_id}:seek", json={
    "topology_version": topology_version,   # fenced against a concurrent rebalance
    "positions": positions,
})
assert resp.status == 200    # 400 OffsetOutOfRange if an integer is out of range: one
# positions violation per entry, reason below_retention_floor or above_high_water_mark
# (the SDK reports it as InvalidInitialPosition).
# 412 topology_version_mismatch if the group rebalanced since JOIN - re-read the
# subscription for the fresh topology_version + assigned set, then re-seek (see §2.2).

# Step 3 — first stream
for frame in http.get(f"/v1/events:stream?subscription={sub_id}", stream=True):
    handle(frame)
# Defensive: broker returns 409 PositionsNotSet if any assigned partition lacks
# a cursor. A well-behaved SDK never sees this on the happy path because Step 2
# seeded every partition. SDK recovery: re-resolve via position() + re-SEEK.

# Broker-side open, in order: the caller's identity (403 TenantIdNotAuthorized for
# a nil subject or tenant id, before any read or PDP call); the subscription (404
# SubscriptionNotFound when it is not a group member) and the caller's right to
# consume as its tenant (403 TenantIdNotAuthorized); the joiner (403
# SubscriptionNotOwned when another principal joined it); the group check, consume
# on the subscription's consumer group, answered from the memo when JOIN, a seek or
# an earlier open left an allow there (404 ConsumerGroupNotFound when the group is
# no longer registered, 403 ConsumerGroupNotAuthorized when the policy refuses it;
# a tenant-gate or group-check answer that does not compile, or a failed
# evaluation, is a 500); the cursors (409 PositionsNotSet); the stream lease (409
# StreamingInProgress); then the member's
# consume scope. The open reuses the scope the member holds when it was evaluated
# for the same principal and no interest group of it has expired. Otherwise it
# evaluates each group as JOIN does, one after another, reusing an answer JOIN or an
# earlier open kept within streaming.authz_scope_ttl_secs (default 60): a kept
# barrier right is not asked again, and a kept consume scope skips both the consume
# scope and the tenant check. A refusal answers the
# JOIN codes (403); a failed evaluation 500; an unavailable candidate query 503. No
# exact-selector check runs at open.
# Worst case: a cold open of many groups with a slow PDP can outlast the member's
# join_timeout (session_timeout after a disconnect); the member is reaped and the
# open answers 404, after which the SDK leaves, re-JOINs and opens again, reusing
# the answers that re-JOIN kept while they are within the TTL.
# An open stream ends with a terminal control frame (reason "teardown") when its
# consume scope reaches the end of its window without a renewal, or when a renewal
# refuses an interest group.

# Broker serves from cursor + 1 (where `cursor` is the session/runtime
# cursor for the subscription's assigned partition). Sentinels resolve at admission:
#   "earliest" → cursor := retention_floor - 1 (emit from retention_floor)
#   "latest"   → cursor := current high-water mark (emit only future events)

# Durability matrix:
#   standalone           → cursor dies with the process
#   cluster + persistent → cursor survives shard restart (Redis-with-disk, etcd)
```

### 2.2 SEEK (set cursor; pre-stream)

`POST /v1/subscriptions/{id}:seek` seeds the starting position for each assigned partition. It is **pre-stream-only**: a SEEK while a `:stream` is open on the subscription is rejected with `409 StreamingInProgress`. Any value in the valid range `[retention_floor - 1, high_water_mark]` is permitted; sentinels `"earliest"` / `"latest"` are server-resolved and returned as integers.

The request carries the `topology_version` the caller last observed (from JOIN or a subscription read). The broker fences the seek against a concurrent rebalance: a value differing from the subscription's current `topology_version` rejects the whole seek with `412 topology_version_mismatch` (nothing seeded), and the caller recovers by re-reading the subscription for the fresh `topology_version` + `assigned` set and re-seeking. This covers the pre-stream JOIN→SEEK window, where no open stream exists to carry an in-band `topology` frame. (Contrast the open-stream rebalance recovery in [`0004-consumption-transport.md`](0004-consumption-transport.md), which reads the fresh assignment from the `topology` frame on the live stream.)

```python
# Consumer side - pre-stream seed (per-partition; values are int OR "earliest"/"latest")
resp = http.post(f"/v1/subscriptions/{sub_id}:seek", json={
    "topology_version": topology_version,   # last observed; fences a concurrent rebalance
    "positions": {
        "orders": [
            {"partition": 0, "value": 42},          # last-processed offset; broker emits the first event above it
            {"partition": 1, "value": "earliest"},  # broker resolves to retention_floor - 1
            {"partition": 2, "value": "latest"},    # broker resolves to the head, the last event's sequence
        ],
    },
})
assert resp.status == 200       # body echoes each position resolved to an integer

# Bulk replay: run it in a dedicated group (e.g. replay-{job-id}) so it never takes
# partitions from live consumers, then SEEK "earliest", an offset, or "at:<time>".

# Broker side
def seek(ctx, sub_id, topology_version, positions):
    if ctx.subject_id.is_nil() or ctx.tenant_id.is_nil():
        return 403_TenantIdNotAuthorized                     # before any read or PDP call
    sub = state.subscriptions.get(sub_id)
    if sub is None:
        return 404_SubscriptionNotFound                      # not a group member
    if not pdp.tenant_check(ctx, sub.tenant_id):
        return 403_TenantIdNotAuthorized                     # the tenant gate
    if sub.joiner != ctx.principal:                          # no PDP call
        return 403_SubscriptionNotOwned
    group = groups.find(sub.group)
    if group is None:
        return 404_ConsumerGroupNotFound                     # no longer registered
    if not pdp.group_check(ctx, group, "consume"):           # an allow in the memo is reused
        return 403_ConsumerGroupNotAuthorized
    if stream_is_open(sub_id):
        return 409_StreamingInProgress                       # SEEK is pre-stream-only
    entries = [(topic, e["partition"], e["value"])
               for (topic, items) in positions.items() for e in items]
    for (topic, _, _) in entries:
        if not topic_registered(topic):
            return 404_TopicNotFound                         # judged before the version fence

    if topology_version != sub.topology_version:
        return 412_TopologyVersionMismatch                   # stale view; re-read + re-seek

    resolved = []                                            # one entry per request entry, in order
    violations = []
    for (topic, part, value) in entries:
        if (topic, part) not in sub.assigned:
            return 409_PartitionNotAssigned                  # the first offender ends the seek
        pos = backend.resolve(topic, part, value)            # int verbatim or sentinel→int
        if pos.partition_missing:
            return 404_PartitionNotFound                     # the backend does not hold it
        if pos.out_of_range:
            violations.append(pos)                           # every offender reported
        else:
            resolved.append(((topic, part), pos))
    if violations:
        return 400_OffsetOutOfRange(violations)              # nothing seeded
    for (key, pos) in resolved:                              # all or none; a partition named twice: the later entry wins
        cursor.set((sub.group, *key), pos)
    return 200_OK(resolved)
```

Sentinel resolution (broker-side, at admission):
- `"earliest"` → cursor set to `retention_floor - 1` for `(group, topic, partition)`. Subsequent emission begins at `retention_floor`.
- `"latest"` → cursor set to the current high-water mark for `(group, topic, partition)`. Subsequent emission includes only events admitted after the SEEK lands.

### 2.3 Re-JOIN after 410 Gone / 404 / shard failover

A stream open on a terminated subscription answers `410 Gone` (not yet answered, tracked in #5977; see [Topology-Change Handling (Rebalance)](0004-consumption-transport.md#topology-change-handling-rebalance) for what a reused id answers today).

```python
# Consumer SDK loop
def wire_positions(group_id, slots):
    """The SEEK body's topic-keyed `positions`: {topic: [{partition, value}]}."""
    out = {}
    for (t, p) in slots:
        out.setdefault(t, []).append(
            {"partition": p, "value": offset_manager.position(group_id, t, p).to_wire()})
    return out

while True:
    try:
        for frame in http.get(f"/v1/events:stream?subscription={sub_id}", stream=True):
            handle(frame)
            if frame.kind == "topology":
                # Re-SEEK newly-assigned partitions (continuing partitions keep their cursor).
                new_slots = [s for s in frame.assigned if s not in prev_assigned]
                if new_slots:
                    http.post(f"/v1/subscriptions/{sub_id}:seek",
                              json={"topology_version": frame.topology_version,
                                    "positions": wire_positions(group_id, new_slots)})
                prev_assigned = frame.assigned
    except (HTTP_410_Gone, HTTP_404_SubscriptionNotFound):
        drop_in_flight(sub_id)                         # any unfinished work for this sub
        sub_id, assigned, topology_version = re_join(group_id, interests)
        # Fresh subscription_id → no SEEK history on broker → must re-seed
        http.post(f"/v1/subscriptions/{sub_id}:seek",
                  json={"topology_version": topology_version,
                        "positions": wire_positions(group_id, assigned)})
        continue
    except TransportError:                             # ungraceful shard kill
        sub_id, assigned, topology_version = re_join(group_id, interests)
        http.post(f"/v1/subscriptions/{sub_id}:seek",
                  json={"topology_version": topology_version,
                        "positions": wire_positions(group_id, assigned)})
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
        # the cluster lock is not yet implemented, tracked in #5979
        # this instance's coordinator lock serializes it today
        gs = GroupState[G]
        # the JOIN first evicts the group's Disconnected members (gs.evict_disconnected_members(), §2.1)
        gs.members[member.id] = member
        rebalance(gs)                                  # range assignment, §3; bumps topology_version
        for m in gs.members.values():                  # the joiner included
            if m.new_assigned != m.assigned:           # a member whose set did not change is skipped
                m.generations.send_replace((gs.topology_version, m.new_assigned))
    # Each open stream whose set changed sees it at once: a lost partition is a
    # topology frame and the stream continues; a gained partition ends the stream
    # with rebalanced; losing every partition ends it with lose_all.

# INVARIANT: no (topic, partition) is processed by two consumers simultaneously.
# The distributed_lock around GroupState mutation serializes the reassignment (not yet implemented, tracked in #5979).
```

### 2.5 Downscaling (remove consumer instance)

```python
# Graceful leave
http.delete(f"/v1/subscriptions/{sub_id}")             # explicit LEAVE
# Broker checks, in order: the caller's identity (403), the subscription (404) and its tenant gate (403), the joiner (403 SubscriptionNotOwned)
# OR - silent death: the consumer's stream drops

# Broker side
def remove_member(G, member_id, reason):               # reason in {"leave", "join_timeout", "session_timeout"}
    with cluster.distributed_lock(f"evbk.group.{G}"):
        # the cluster lock is not yet implemented, tracked in #5979
        # this instance's coordinator lock serializes it today
        gs = GroupState[G]
        del gs.members[member_id]
        # a departed member still streaming (a LEAVE) gets an empty assignment: its stream ends with lose_all
        rebalance(gs)                                  # redistribute departed member's partitions; bumps topology_version
        for survivor in gs.members.values():
            if survivor.new_assigned != survivor.assigned:   # unchanged sets are skipped
                survivor.generations.send_replace((gs.topology_version, survivor.new_assigned))
    # In-flight events held in the departed member's buffer are NOT shipped
    # elsewhere - after its re-JOIN the new owner's SDK SEEKs from its own offset store
    # and reads from cursor.offset + 1 (the consumer's store is the authority, ADR-0006).

# Reaper fires the silent-death path:
async def reaper_loop():
    while True:
        await sleep(1)
        for sub in state.subscriptions.values():
            if sub.state == Joined and sub.join_timeout_lapsed():
                remove_member(sub.group, sub.id, reason="join_timeout")
            elif sub.state == Disconnected and sub.session_timeout_lapsed():
                remove_member(sub.group, sub.id, reason="session_timeout")
            # a Streaming member is never reaped
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
#   ← the cursor holds what the group's last SEEK wrote; delivery does not move it
#   (R60 in DESIGN.md: accepted rollout behavior)

# When all v1 members leave, the group settles into the F2/T2 worldview.
```

- **Scanned-past events stay behind.** Events F1 rejected but F2 would accept, and that F1 already scanned past, are never seen by F2: the cursor has moved beyond them. Gradual rollouts trade strict filter consistency for zero downtime; an operator wanting strict semantics drains v1 before starting v2.
- **No group-level filter.** Enforcing filter parity across a group would force either rejecting v2's JOIN or evicting v1 members on a filter change. Per-member filters keep both rolling deploys and hard-stop deploys possible.

### 2.7 Session timeout

```python
# A member's lifetime depends on its state:
#   Joined        - reaped after subscription.join_timeout_secs (default 60) if no stream opens
#   Streaming     - no limit while its stream is open
#   Disconnected  - its stream closed; reaped after the member's own session_timeout
#   a new JOIN to the group evicts its Disconnected members at once
def on_stream_closed(sub_id):
    sub = state.subscriptions[sub_id]
    sub.state = Disconnected            # session_timeout counts from here
    # it keeps its partitions, so a reconnect within the window resumes without a rebalance, unless a JOIN evicted it first

# The sweep (see §2.5) removes members whose window lapsed and triggers a rebalance.
# The consumer's next request after the reap returns 404 SubscriptionNotFound;
# the consumer re-JOINs per §2.3.
```

### 2.8 Delivery shard ownership change

```python
# Shard ownership handover and failover are not yet implemented, tracked in #4438.

# Graceful release (e.g., shard A scaling down)
async def release_ownership(G):
    for stream in streams_of_group(G):
        respond_410_gone(stream)                       # consumer will re-JOIN
        # 410 not yet answered, tracked in #5977; a reaped or left member's
        # subscription answers 404 SubscriptionNotFound.
    await cluster.distributed_lock.release(f"evbk.group.{G}")
    # Next shard B to acquire the lock claims ownership and runs a fresh rebalance.

# Ungraceful kill (shard A forcibly terminated)
#   - Open streams see TCP RST or read timeout
#   - cluster.distributed_lock has a session TTL; the lock auto-releases on TTL expiry
#   - Shard B acquires the lock; reconstructs GroupState from cache (cluster) or
#     starts fresh (standalone or non-persistent cache)
#   - Existing sub_ids in cache (not yet implemented, tracked in #5979) are
#     honored if session_timeout hasn't elapsed;
#     otherwise treated as gone (consumer re-JOINs)

# Cursor durability across ownership change:
#   standalone           → no shard concept; broker restart = full cache wipe
#   cluster + persistent → cursor.offset survives via Redis-with-disk / etcd
```

## 3. Processes / Business Logic (CDSL)

The lock around `cluster.distributed_lock("evbk.group.<G>")` is the serialization point for every GroupState mutation (JOIN, LEAVE, rebalance, ownership transfer) (not yet implemented, tracked in #5979). Stream reads and pre-stream SEEK are NOT lock-protected by the GroupState mutation lock - they touch runtime cursor cache keys directly and rely on the cache's own consistency guarantees.

`topology_version` is monotonically increasing per group; every mutation increments it. Consumers detect a rebalance from the `topology_version` carried in every `topology` frame, or from a stream that ends with `rebalanced` (a gained partition) or `lose_all` (every partition lost).

**Rebalance (v1): range assignment per topic.** Runs under the group lock on every JOIN, LEAVE and reap (a lapsed `join_timeout` or `session_timeout`), and recomputes the whole assignment:

```python
def rebalance(gs):
    # every member, Disconnected included (the coordinator's rebalance)
    members = sorted(gs.members.values(), key=lambda m: m.subscription_id)   # deterministic
    for topic in union(m.topics for m in members):
        eligible = [m for m in members if topic in m.topics]     # only members subscribed to it
        n, k = topic.partitions, len(eligible)
        if k == 0:
            continue                                             # no eligible member: unassigned
        for i, m in enumerate(eligible):                         # member i gets [i*n/k, (i+1)*n/k)
            for p in range(i * n // k, (i + 1) * n // k):        # integer division; empty for some members when k > n
                assign(topic, p, m)
        # n=5, k=2: [0, 1] and [2, 3, 4]; n=2, k=4: members 1 and 3 get one partition each
    gs.topology_version += 1
```

- **Thrash is accepted in v1.** A full recompute can move nearly every partition on every membership change. At-least-once delivery with idempotent consumers absorbs it: work in flight on a moved partition is delivered again by the new owner. Sticky assignment, which minimizes movement, is deferred (DESIGN §4.8); `topology_version` already carries the contract, so it is an internal change.

A subscription carries a `created_at` timestamp set at JOIN. `GET /v1/subscriptions` returns subscriptions newest-first, ordered by `created_at` descending with `id` as a stable tiebreaker, so a freshly created subscription lands on the first page rather than wherever its random id would fall.

## 4. States (CDSL)

| State | Trigger | Next |
|---|---|---|
| Joined | POST /v1/subscriptions accepted (JOIN); the rebalance assigns its partitions | Streaming (a stream opens) / Reaped (join_timeout elapses with no stream opened) |
| Streaming | GET /v1/events:stream or /v1/events:sse open | Disconnected (the stream closes) |
| Disconnected | the stream closed; partitions held | Streaming (a stream reopens within session_timeout) / Reaped |
| Reaped | join_timeout elapsed with no stream opened, session_timeout elapsed after the stream closed, or a new JOIN evicted the Disconnected member | Gone |
| Gone | LEAVE or Reap | terminal — must re-JOIN to recover |

## 5. Definitions of Done

### Broker-Side Implementation

- [ ] `p1` - **ID**: `cpt-cf-evbk-dod-consumer-subscription-lifecycle-broker`

- DispatcherService routes by `consumer_group` GTS id to the owning shard (not yet implemented, tracked in #4438).
- DeliveryService implements the v1 rebalance algorithm under `cluster.distributed_lock` (not yet implemented, tracked in #5979).
- `GroupState` carries per-member filters / topic lists; rebalance respects them.
- `cursor.offset` is updated atomically via cache CAS (compare-and-swap with `>=` semantics; only SEEK writes it, before the stream opens, and SEEK accepts any value in the valid range; delivery never moves it).
- `topology_version` is carried in every `topology` frame, or signalled by a stream that ends with `rebalanced` (a gained partition) or `lose_all` (every partition lost).
- Reaper removes members past their state's lifetime (`join_timeout` before the first stream, `session_timeout` after a stream closes) and triggers rebalance.
- The eight flows above are implemented end-to-end and visible through `evbk_*` metrics.

## 6. Acceptance Criteria

- **AC-1 (cold JOIN)**: POST /v1/consumer-groups with `{ "client_agent": "<valid User-Agent string>" }` returns 201 + Location + broker-minted id + echoed `client_agent`; subsequent POST /v1/subscriptions referencing that id succeeds. POST without `client_agent` is rejected with the canonical RFC 9457 400 problem type.
- **AC-2 (seek)**: SEEK (`POST /v1/subscriptions/{id}:seek`) on assigned partitions updates `cursor.offset`; the stream then delivers events from the new offset; SEEK on unassigned partition returns 409.
- **AC-3 (re-JOIN)**: a consumer whose stream receives 410 Gone (not yet answered, tracked in #5977; a reaped or left member's subscription answers `404 SubscriptionNotFound`) successfully re-JOINs with a fresh subscription id and resumes consumption.
- **AC-4 (upscale)**: adding member N+1 causes existing members to receive a new `topology_version` and updated `assigned` set within 5s.
- **AC-5 (downscale)**: graceful LEAVE redistributes partitions within 5s; silent death takes effect within `session_timeout` + the detection delay + one sweep interval (1 s), as AC-7 defines them.
- **AC-6 (filter mutation)**: a partition handoff from v1 to v2 member preserves the cursor; no events are double-delivered to v1 after handoff.
- **AC-7 (session timeout)**: a consumer whose stream has been closed for `session_timeout` + the detection delay + one sweep interval (1 s) finds its subscription reaped on its next request; re-JOIN succeeds. The detection delay is how long the broker takes to see the close: none when the client closes the connection (the server reads its end at once), otherwise at least one heartbeat interval (5 s by default), until TCP reports the peer gone and a write fails.
- **AC-8 (shard ownership change)**: graceful shutdown of owning shard delivers 410 to all open streams within 1s (not yet answered, tracked in #5977; a reaped or left member's subscription answers `404 SubscriptionNotFound`); ungraceful kill results in transport error within the cluster lock's session TTL (not yet implemented, tracked in #4438).
- **AC-9 (typed filter happy path)**: JOIN with an interest carrying `filter: { engine: "gts.cf.core.events.filter.v1~cf.core.expression.cel.v1", expression: "event.data.amount > 100" }` succeeds; the stream then delivers only events matching the predicate; non-matching events are silently dropped (not yet implemented, tracked in #5978).
- **AC-10 (no filter)**: JOIN with an interest omitting `filter` succeeds; the stream then delivers every event matching topic + tenant + types that the caller's consume scope admits (no engine invocation).
- **AC-11 (malformed filter object)**: JOIN with an interest supplying a `filter` object missing `engine` or `expression` is rejected with `422` by the REST handler's body deserialization.
- **AC-12 (invalid type pattern)**: JOIN with `types: ["gts.cf.core.events.event.v1~yourorg.*.placed.v1"]` (mid-pattern wildcard) is rejected with `400 BadTypePattern`.
- **AC-13 (zero-match type pattern)**: JOIN with a `types[]` pattern matching no registered types under the declared topic returns `201`, and that interest's `unmatched_types` lists the pattern; a read of the subscription does not carry `unmatched_types`.
- **AC-14 (type-belongs-to-topic)**: JOIN with a `types[]` pattern that would resolve to a type whose `parent_topic` differs from `interest.topic` is rejected with `400 TypeNotInTopic` (defense-in-depth) (not yet implemented, tracked in #5978).
- **AC-15 (rolling-deploy correctness)**: members M1 (interests pointing at `placed.v1`) and M2 (interests pointing at `placed.v2`) coexist in one group; rebalance produces correct per-member assignments; events of `placed.v1` ship only to M1; events of `placed.v2` ship only to M2.
- **AC-16 (filter eval timeout drops, does not fail the stream)**: an interest whose `engine.eval` exceeds `EVAL_TIMEOUT_MICROS` produces a warn log + `evbk_filter_eval_timeout_total{consumer_group}` increment; the event is dropped from this consumer's batch and the stream does not fail (not yet implemented, tracked in #5978).

## 7. Unit Test Plan

- **rebalance algorithm**: parameterize over (member counts, topic subsets, partition counts); assert single-consumer-per-partition invariant + per-member topic respect.
- **cursor SEEK, no forward-only rule**: a SEEK with an offset older than the current `cursor.offset` moves the cursor back; a SEEK while a stream is open on the subscription is refused `409 StreamingInProgress`.
- **topology_version monotonicity**: each mutation increments by exactly 1; concurrent mutations serialize through the lock.
- **filter evaluation order**: per-member filter applies AFTER partition assignment; events for non-assigned partitions are never evaluated.
- **interest prerequisites**: per-event delivery checks the event's topic - resolved from the `topic` trait on its `type`, not carried on the event - against `interest.topic`, then the interest's ownership narrowing (`only_mine`: `event.subject_tenant` is the caller's tenant and `event.subject_owner` is the caller; `entire_tenant`: `event.subject_tenant == interest.subject_tenant`; `tenant_subtree_included`: none), then `event.type ∈ resolved_type_set`, then, when the interest carries `subject_types`, `event.subject_type` matching one of them, then the member's consume scope admitting the event, BEFORE invoking `engine.eval` (not yet implemented, tracked in #5978). Non-matching prerequisite → skip interest without engine call.
- **filter-object shape**: parameterize over `filter` ∈ {absent, `{engine, expression}`, `{engine}` only, `{expression}` only} - absent and complete accepted; partial (missing `engine` or `expression`) rejected with `422` by the REST handler's body deserialization.
- **GTS pattern syntax**: parameterize over valid (exact, trailing `.*`, trailing `~*`, `v*` at trailing) and invalid (mid-pattern `*`, `**`, substring `vendor*`, two `*` occurrences) patterns; valid → accept; invalid → `400 BadTypePattern`.
- **Version resolution**: parameterize over (registered types, pattern) tuples and assert resolved set matches per-name-latest + minor-version-omitted rule from ADR-0005.
- **Filter context (CEL engine)**: assert `event.id` / `event.type` / ... visible; `event.meta` reference → `FilterError::CompileFailed` (not yet implemented, tracked in #5978).

## 8. E2E Test Plan

- **S1 - full lifecycle**: POST /v1/consumer-groups → JOIN → SEEK → stream → SEEK (advance cursor) → stream → LEAVE; verify every state transition.
- **S2 — concurrent JOINs**: 100 members JOIN within 1s; verify all assigned at least one partition; verify no partition is double-assigned.
- **S3 — rolling deploy**: start with 5 v1 members; introduce 5 v2 members one at a time; remove v1 members; assert no event is lost, no event is double-processed.
- **S4 - shard kill**: ungracefully kill owning shard; another shard takes over within the cluster lock TTL (not yet implemented, tracked in #4438); consumers re-JOIN and resume.
- **S5 - proxy idle behavior**: place a 25s-idle-timeout proxy between consumer and broker; verify the stream's heartbeats (default every 5 s) keep it open through the proxy.
- **S6 — standalone cursor loss**: in standalone mode, restart broker; subscription gone, cursor gone; new JOIN resumes from `earliest_available_offset`.
- **S7 - cluster cursor durability**: in cluster mode with persistent cache provider, restart owning shard; new shard takes over (not yet implemented, tracked in #4438); cursor preserved; consumer resumes from `cursor.offset + 1` (i.e., SEEK sets `cursor.offset`, broker emits from `cursor.offset + 1`).
- **S8 - session timeout reap**: configure `session_timeout=PT3S`; open the stream, then close it; after `session_timeout` + the detection delay + 2s (5s when the test closes the connection), open it again; verify `404 SubscriptionNotFound`; re-JOIN succeeds.
