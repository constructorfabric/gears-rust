# Event Broker — Consumer Use Cases & Flows

This document captures consumer-side state machines, producer / consumer profiles, walked scenarios, and the v1 rebalance algorithm. It is a non-canonical extension to [DESIGN.md](DESIGN.md) — the canonical DESIGN template does not prescribe a section for this content; it is preserved as a separate file so DESIGN.md stays aligned with the canonical structure.

**ID**: `cpt-cf-evbk-consumer-flows`

The protocol is intentionally aligned with the **Confluent REST Proxy v2 / Strimzi Kafka Bridge** convention — the de-facto Kafka REST contract shared by Confluent REST Proxy, Strimzi Kafka Bridge (CNCF, Apache 2.0), Karapace (Apache 2.0), and Redpanda Pandaproxy. This document walks through the canonical consumer flows.

## 1. Consumer State Machine

```
  ┌──────────────────────┐
  │  No subscription     │  ← initial / after explicit DELETE
  └──────────┬───────────┘
             │ POST /v1/subscriptions { consumer_group, client_agent, interests: [{topic, tenant_id, types, ...}] }
             │   ↳ broker triggers rebalance for the group
             │   ↳ if other consumers are in the group, they receive a
             │     topology_change wake-up on their next poll
             ▼
  ┌──────────────────────────────────────────────┐
  │  Joined; assignments may be empty             │
  │  - id, topology_version,                      │
  │    assignments: [{partition, offset}, ...]    │
  └──────────┬───────────────────────────────────┘
             │ GET /v1/events:stream?subscription_id=...
             ▼
  ┌──────────────────────────────────────────────┐
  │  Polling                                      │
  │  Server holds: cursor.{offset, sent} per       │
  │                (subscription_id, partition)   │
  │                                               │
  │  Poll wakes on ANY of:                        │
  │   - event published to assigned partition     │
  │   - topology change (rebalance happened)      │
  │   - timeout                                   │
  │                                               │
  │  Response always includes:                    │
  │   { items, topology_version,                  │
  │     assignments: [{partition, offset}, ...],  │
  │     retry_after_seconds? }                    │
  └─┬──────────────────┬─────────────────────────┘
    │                  │
    │ topology_version │ assignment unchanged
    │ changed          │
    ▼                  ▼
  Re-align           Continue polling
  (assigned_         (process items,
   partitions        commit offsets,
   shifted)          poll again)
```

## 2. Producer Use Cases

The producer flows we have explicitly accounted for:

| # | Producer profile | Producer-Id strategy | Idempotency story |
|---|---|---|---|
| **P1** | Has DB + uses outbox | Persistent UUID stored alongside business data | Outbox tracks `last_sent_sequence` per `(topic, partition)`; transactionally atomic with business writes; exactly-once via outbox replay on retry |
| **P2** | Has DB, no outbox | Persistent UUID stored alongside business data | Application persists `last_sent_sequence` in own DB; idempotent producer protocol via Producer-Id header; reads broker's `last_sequence` on startup if local state is lost |
| **P3** | Stateless single instance | Ephemeral (fresh UUID per process startup) | New PID each restart; can never deduplicate across restarts; relies on consumer-side idempotent processing |
| **P4** | Stateless burst (lambda-like) | No `Producer-Id` header at all | Stateless mode — broker does no dedup; at-least-once delivery; consumer-side processing MUST be idempotent |
| **P5** | Multi-instance shared service | Each instance generates its OWN UUID at startup | No PID sharing — instances never collide; partition selection (key-hash) routes related events to consistent partitions |
| **P6** | Bulk backfill / replay | Either ephemeral PID or no PID (mass writes) | Application-managed dedup; out-of-order events tolerable for backfill; consumer must be idempotent |

**Rule for P5**: Two producer instances MUST use distinct `producer_id`s — and since each instance calls `POST /v1/producers` independently and gets its own server-issued UUID bound to its principal, this is automatic. If two instances did somehow share a UUID (e.g., both restored from the same persisted blob without rotation), they'd race the chain — the unique constraint on `evbk_producer_state(producer_id, topic, partition)` serializes them, the loser sees `412 SequenceViolation`, and the recommended recovery is to register a fresh `producer_id` (no epoch needed).

## 3. Consumer Use Cases

| # | Consumer profile | Behavior | Recovery / scale |
|---|---|---|---|
| **C1** | Single, DB-backed, solo | Joins group with itself as sole member, gets all partitions, processes & commits in own DB transaction (broker's `cursor.offset` is a checkpoint, consumer's own DB is source of truth) | On restart: same Producer-Id, server recreates assignment if `session_timeout` expired or resumes if still active |
| **C2** | Single, stateless, solo | Joins group, polls, broker tracks only runtime cursor state | On crash: `session_timeout` expires; new instance joins → gets all partitions → re-initializes position from its chosen start point and may re-receive uncommitted events |
| **C3** | Group, multiple instances | Each instance creates its own subscription with same `consumer_group` | Broker rebalances on each JOIN/LEAVE/expiry; partitions distributed round-robin (sticky-Kafka deferred to v2) |
| **C4** | Group, planned upscale | New instance: `POST /v1/subscriptions` | Existing polls wake on topology change, return with new (smaller) `assignments`; new instance gets its share immediately |
| **C5** | Group, planned downscale | Departing instance: `DELETE /v1/subscriptions/{id}` | Remaining instances' polls wake; partitions reassigned to survivors via rebalance |
| **C6** | Group, crash downscale | No DELETE — `session_timeout` expires; Reaper detects and triggers rebalance | Same as C5 but delayed by `session_timeout` (default `PT30S`) |
| **C7** | Bulk historical replay | Join with a dedicated `consumer_group` (e.g., `replay-{job-id}`), then `POST /subscriptions/{id}:seek { "partition_positions": {"P":0} }` to seek to start, then poll | Standard subscription path; the dedicated group ensures replay traffic doesn't compete with primary consumers. Anonymous offset-based read is **out of scope for MVP** — see DESIGN.md §4.6/§4.8. |
| **C8** | Filtering-heavy | `types`, `subject_types`, `cel` filters declared at JOIN | Filters applied server-side before returning; reduces network traffic |
| **C8a** | Rolling deploy with filter or topic change | New version of consumer service deploys with different `types`/`cel` filter set, or expanded/reduced `topics` list | Per-member subscriptions: each member's JOIN succeeds regardless of differences from existing members. Partition assignments span the union of all members' topic lists; `(topic, partition)` is assigned only to a member that subscribes to that topic. During the transition, partitions migrate organically from v1 to v2 instances; the **single-consumer-per-partition invariant** holds at every moment. The cursor reflects whichever member processed events at the time of advancement; events at the migration boundary may be filtered differently than steady-state. Operator can hard-stop deploy (drain v1, deploy v2) for strict atomic semantics. |
| **C9** | Hot-standby (active/passive) | Multiple instances, same consumer_group, but only one instance polls at a time (the active one); standby is a placeholder subscription | Active fails → its `session_timeout` expires → standby's next poll returns the reassigned partitions |

### Filter Behavior During Rollout

The broker does **not** enforce canonical filters at the group level. Each member's JOIN declares its own filters (`types`, `subject_types`, `cel`) and topic list (`topics`). Different members in the same group can have different filters and different topic lists — this is intentional, to support gradual rolling deploys.

**Invariants that ARE enforced**:
- Single-consumer-per-`(topic, partition)` at any moment: a partition is owned by exactly one active subscription
- A `(topic, partition)` is assigned only to a member whose `topics` list includes that `topic`
- Cursor `(consumer_group, topic, partition)` advances (via SEEK) based on the current owner's processing

**Behavior on rolling deploy** (v1 with filters F1, v2 with filters F2 coexisting):
- Some `(topic, partition)` pairs owned by v1 instances → filtered with F1
- Some pairs owned by v2 instances → filtered with F2
- When a partition migrates from F1-owner to F2-owner via rebalance, the cursor stays where F1 left it; F2 starts reading from there
- Events that F1 rejected but F2 would accept may have been already scanned-past by F1 (tracked via `last_examined`); F2 will not see them
- This is **accepted behavior**: gradual rollouts trade strict filter consistency for zero-downtime evolution

**For strict atomic semantics**: operators perform a hard-stop deploy — drain all v1 instances (k8s rollout, group session_timeout, or explicit DELETE), then start v2. During the brief gap, no consumer is processing; cursors are unchanged; v2 starts consuming from the exact position v1 left.

**Why no canonical filter at the group level**: enforcing filter parity would require either rejecting v2's JOIN (forcing rename) or kicking out v1 instances on filter change (downtime). Per-member filtering supports both fast rolling deploys and operator-driven hard-stop semantics, with the trade-off documented and discoverable.

## 4. Walked Scenarios

**Scenario A — 1 partition, 2 instances (single-topic group):**

```
T0: Instance A: POST /v1/subscriptions {consumer_group: "gts.cf.core.events.consumer_group.v1~vendor.audit.v1", client_agent: "audit-svc/1.0", interests: [{topic: "T", tenant_id: ..., types: ["gts.cf.core.events.event.v1~*"]}]}  (T has 1 partition)
    Dispatcher: cache.put_if_absent("evbk.group.endpoint:{vendor.audit.v1}", thisInstance)
                (claims ownership of group GTS ~vendor.audit.v1)
    Broker: rebalance → A gets (T, 0)
    Response 201: { id: A, topology_version: 1,
                    assignments: [ {topic:"T", partition:0, offset:0, last_examined:0} ] }
    A: GET /v1/events:stream?subscription_id=A  ← streams (T, 0)

T1: Instance B: POST /v1/subscriptions {consumer_group: "gts.cf.core.events.consumer_group.v1~vendor.audit.v1", client_agent: "audit-svc/1.0", interests: [{topic: "T", tenant_id: ..., types: ["gts.cf.core.events.event.v1~*"]}]}
    Dispatcher: cache.get("evbk.group.endpoint:{vendor.audit.v1}") → existing endpoint, forward there
    Owning instance: rebalance → A keeps [(T,0)], B gets []
    Response 201: { id: B, topology_version: 2,
                    assignments: [], retry_after_seconds: 5 }
    cluster.publish("evbk.group.{vendor.audit.v1}.topology", v=2)

T2: A's stream wakes on topology v=2
    A re-reads its assignment: still [(T, 0)]
    A returns 200: { topology_version: 2,
                     assignments: [{topic:"T", partition:0, offset:0, last_examined:0}],
                     batches: [] }
    A SDK: assignment unchanged, resume polling

T3: B is in retry-after loop (sleep 5s, or hold connection on topology channel)

T4: A's session expires (crash or DELETE)
    Reaper triggers rebalance → B gets (T, 0)
    cluster.publish topology v=3

T5: B wakes, polls (B's offsets reflect A's last sought position — cursor is in cache, sticky)
    B's response: { topology_version: 3,
                    assignments: [{topic:"T", partition:0, offset:<A's offset>, last_examined:<A's last_examined>}],
                    batches: [{topic:"T", partition:0, events:[<events with offset > A's offset>]}] }
    Events polled-but-not-sought-past by A are re-delivered to B (at-least-once)
```

**Scenario B — 8 partitions, 2 instances arriving back-to-back:**

```
T0: A: POST /v1/subscriptions {consumer_group: "gts.cf.core.events.consumer_group.v1~vendor.audit.v1", client_agent: "audit-svc/1.0", interests: [{topic: "T", tenant_id: ..., types: ["gts.cf.core.events.event.v1~*"]}]}  (T has 8 partitions)
    Dispatcher: claims ownership of G via cache.put_if_absent
    Broker rebalance: A → [(T,0)..(T,7)]
    Response 201: { id: A, topology_version: 1,
                    assignments: [
                      {topic:"T", partition:0, offset:0, last_examined:0},
                      ..., 
                      {topic:"T", partition:7, offset:0, last_examined:0}
                    ] }
    A: GET /v1/events:stream?subscription_id=A  ← streams all 8

T1: B: POST /v1/subscriptions {consumer_group: "gts.cf.core.events.consumer_group.v1~vendor.audit.v1", client_agent: "audit-svc/1.0", interests: [{topic: "T", tenant_id: ..., types: ["gts.cf.core.events.event.v1~*"]}]}
    Dispatcher: cache.get("evbk.group.endpoint:{vendor.audit.v1}") → owning instance, forward
    Owning instance, rebalance under cluster.distributed_lock("evbk.group.{vendor.audit.v1}.rebalance"):
      Validate B's topics list ["T"] is acceptable (per-member; broker doesn't enforce match)
      B's filters apply to B alone (per-member; no canonical group filter)
      Active subs (sorted by created_at): [A, B]
      Round-robin split: A → [(T,0)..(T,3)], B → [(T,4)..(T,7)]
      Update group_state in cache:
        active_members[A].assigned = [(T,0)..(T,3)]
        active_members[B].assigned = [(T,4)..(T,7)]
      Initialize cursor cache entries for (G, T, 4..7) if missing
        (no-op if entries already exist — cursors are group-keyed, sticky in cache)
      cluster.publish topology v=2
    Response 201: { id: B, topology_version: 2,
                    assignments: [ {topic:"T", partition:4, offset:<A's offset on 4>, ...},
                                   {topic:"T", partition:5, offset:<A's offset on 5>, ...},
                                   {topic:"T", partition:6, offset:..., ...},
                                   {topic:"T", partition:7, offset:..., ...} ] }

T2: A's stream wakes on topology v=2
    A re-reads its assignment: now [(T,0)..(T,3)]
    A returns 200: {
      topology_version: 2,
      assignments: [ {topic:"T", partition:0, offset:<A's offset>, ...}, ...,
                     {topic:"T", partition:3, offset:<A's offset>, ...} ],
      batches: [{topic:"T", partition:N, events:[...]} for any pending events on T,0..T,3]
    }
    A SDK: assignment shrunk → re-poll with awareness of new state

T3: B polls
    B: GET /v1/events:stream?subscription_id=B
    Dispatcher: resolve B → consumer_group GTS; cache.get("evbk.group.endpoint:{vendor.audit.v1}") → owning instance, forward
    Owning instance reads cursor[(G,T,4..7)].offset — sticky offsets preserved
    B gets events from (T, 4..7) starting at those offsets
    Includes anything A polled-but-didn't-seek-past (at-least-once redelivery)

T4: B processes events, seeks past them via:
    POST /v1/subscriptions/B:seek { "partition_positions": [{"topic":"T","partition":4,"value":130},{"topic":"T","partition":5,"value":78}] }
```

**Scenario C — Producer DB restored from backup (Producer-Id rotation):**

```
T0: Producer running with UUID PID_1 = "550e8400-e29b-41d4-a716-446655440000"
    Producer's local DB: { producer_id: PID_1, last_sent_sequence[topic, 4] = 1000 }
    Broker:               last_sequence(PID_1, topic, 4) = 1000

T1: Producer DB restored from yesterday's backup
    Producer's local DB: { producer_id: PID_1, last_sent_sequence[topic, 4] = 800 }   ← stale
    Broker still has:     last_sequence(PID_1, topic, 4) = 1000

T2: Application detects stale state on startup
    (e.g., business data has rows newer than last_sent_sequence claims, or a
     sentinel row marks a known-good restore point that doesn't match)

T3: Application generates new UUID PID_2 = "a1b2c3d4-..."
    Persists alongside business data:
      producer's local DB: { producer_id: PID_2, last_sent_sequence[topic, 4] = 0 }

T4: Producer publishes with Producer-Id: PID_2
    Broker has no state for PID_2 → first publish creates state at sequence 1
    Producer continues normally with fresh PID
    
T5: Reaper eventually deletes evbk_producer_state rows for PID_1
    after the topic's retention (no activity from PID_1 anymore)
```

This is the canonical reset flow — application detects stale state, rotates UUID, broker treats it as a brand-new producer. No explicit reset API needed.

## 5. Rebalance Algorithm (v1)

Round-robin per topic, sorted by `(created_at, id)` for determinism, **respecting per-member topic subscriptions**. Sticky-Kafka (minimize partition movement on rebalance) is deferred to v2 — contracts already carry `topology_version` so this is an internal change, not a contract change.

**v1 thrash is acceptable.** Round-robin recomputes the assignment from scratch on every JOIN/LEAVE, which means nearly every partition can move on every membership change. For groups with many partitions and frequent membership churn, this is wasteful — but at-least-once delivery + idempotent consumers absorbs the cost: any work in-flight when a partition moves is simply re-delivered by the new owner. Sticky-Kafka assignment is the optimization that minimizes this and is captured in DESIGN.md §4.8 Future Developments.

```
rebalance(group G):  # G is the GTS-typed consumer_group identifier
  acquire cluster.distributed_lock("evbk.group.{G}.rebalance", ttl=10s)
  try:
    group_state = cache.get("evbk.group.{G}")
    active_subs = group_state.active_members.values()
                    .filter(s => s.expires_at > now())
                    .sorted_by(s => (s.created_at, s.id))

    # Compute group's effective topic set: union of all members' topics
    effective_topics = union(sub.topics for sub in active_subs)

    new_assignments = {}  # (topic, partition) -> subscription_id

    # Round-robin per topic, considering only members that subscribe to it
    for topic_T in effective_topics:
      eligible = [sub for sub in active_subs if topic_T in sub.topics]
      N = topic_T.partitions
      S = len(eligible)
      
      if S == 0: continue  # no member subscribes; partitions orphaned
      
      if S >= N:
        for i in 0..N: new_assignments[(topic_T, i)] = eligible[i].id
      else:
        base, extra = N / S, N % S
        cursor = 0
        for i in 0..S:
          count = base + (1 if i < extra else 0)
          for p in cursor..cursor+count:
            new_assignments[(topic_T, p)] = eligible[i].id
          cursor += count

    # Apply assignments to group_state.active_members
    for sub_id in active_subs.map(s => s.id):
      group_state.active_members[sub_id].assigned = [
        (topic, partition) for (topic, partition), assigned_id in new_assignments
                            if assigned_id == sub_id
      ]
    
    # Initialize cursor cache entries for any (G, topic, partition) tuples newly assigned
    # Existing offset values are preserved automatically (group-keyed, sticky in cache)
    for (topic_T, partition) in new_assignments.keys():
      cache.put_if_absent("evbk.cursor:{G}:{topic_T}:{partition}",
                          { offset: 0, last_examined: 0, updated_at: now() })
    
    # No "cursor.sent reset" needed — sent is in-memory per-subscription state in cache,
    # naturally reset when partitions migrate to a new subscription
    
    group_state.topology_version += 1
    cache.put("evbk.group.{G}", group_state, ttl=max(member.expires_at))
    
    cluster.publish("evbk.group.{G}.topology",
                    { version: group_state.topology_version, changed_at })
  finally:
    release lock
```

**Triggers**: `POST /v1/subscriptions` (JOIN), `DELETE /v1/subscriptions/{id}` (LEAVE), Reaper detecting `expires_at < now()` (CRASH).

**Concurrency**: Multiple JOINs in flight serialize on the lock; each runs a fresh rebalance with the latest membership.

**At-least-once contract**: When partitions migrate from sub A to sub B, `cursor.offset` is preserved (sticky); `cursor.sent` is reset to `cursor.offset` so B re-receives any events A polled but didn't seek past. Consumers MUST be idempotent in their processing.
