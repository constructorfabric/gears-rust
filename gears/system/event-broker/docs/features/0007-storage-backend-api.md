# Feature: Storage Backend API

- [1. Feature Context](#1-feature-context)
  - [1.1 Overview](#11-overview)
  - [1.2 Purpose](#12-purpose)
  - [1.3 Actors](#13-actors)
  - [1.4 References](#14-references)
- [2. Backend Contract](#2-backend-contract)
  - [2.1 Trait](#21-trait)
  - [2.2 Append](#22-append)
  - [2.3 Read](#23-read)
  - [2.4 Resolve](#24-resolve)
  - [2.5 Watch](#25-watch)
  - [2.6 Provision and Query](#26-provision-and-query)
  - [2.7 Provider](#27-provider)
  - [2.8 Errors](#28-errors)
- [3. Processes / Business Logic (CDSL)](#3-processes--business-logic-cdsl)
  - [3.1 Ingest Write Path](#31-ingest-write-path)
  - [3.2 Head Cache](#32-head-cache)
  - [3.3 Delivery Reads](#33-delivery-reads)
- [4. Meeting the Contract on a Medium](#4-meeting-the-contract-on-a-medium)
- [5. Actor Flows (CDSL)](#5-actor-flows-cdsl)
  - [5.1 Stored Publish on a Backend That Returns Heads](#51-stored-publish-on-a-backend-that-returns-heads)
  - [5.2 Publish on a Backend That Assigns Later](#52-publish-on-a-backend-that-assigns-later)
  - [5.3 A Reader at the Tail](#53-a-reader-at-the-tail)
- [6. Open Items](#6-open-items)
- [7. Out of Scope](#7-out-of-scope)
- [8. Definitions of Done](#8-definitions-of-done)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Unit Test Plan](#10-unit-test-plan)
- [11. E2E Test Plan](#11-e2e-test-plan)

## 1. Feature Context

### 1.1 Overview

The trait every storage backend plugin implements, and how ingest and delivery drive it.

The backend owns a topic's log: it picks each event's partition from the event's partition key and assigns its sequence. Ingest hands events over and learns partition heads afterwards; delivery reads by offset. Backends differ in what they can do - return positions in the write acknowledgement or assign them later, push head changes or be polled - and the contract admits each of them without a second code path in the gear.

### 1.2 Purpose

- One trait for every medium: a database, a directory, a Kafka cluster, a stream server.
- The backend, not the gear, assigns partition and sequence.
- A write returns only once the events are durably stored, whatever the backend does after.
- One read operation, by offset, for random reads and the tail alike.
- Head changes observable per partition, pushed where the medium can and polled where it cannot.

### 1.3 Actors

- **Ingest instance**: appends events drained from its outbox, answers stored publishes (`Prefer: wait`) once their batch is stored, and keeps the head cache.
- **Delivery instance**: reads partitions by offset to fill its partition cache, waking on the head cache.
- **Storage backend plugin**: implements the trait over its medium; how it talks to the medium (connection, batching, flush schedule) is its own configuration.

### 1.4 References

- Feature 0005 - backend registry and routing: which backend a topic resolves to
- Feature 0006 - topic provisioning: `provision`, the log id
- ADR-0001 - offset semantics: sequences start at 1, the offset is the last-processed position
- ADR-0002 - partition selection: the partition key is the value at the event type's JSON Pointer; the backend maps it to a partition
- ADR-0004 - idempotent producer protocol: chain state per `(producer_id, topic)`, checked before enqueue
- `event-broker-sdk/src/sequence.rs` - `Sequence`
- `libs/toolkit-db/src/outbox/core.rs` - `Outbox::subscribe`, traced batches
- `event-broker/src/domain/notify.rs` - the payload-free notification the head cache replaces
- `event-broker/src/infra/loader/scheduler.rs` - the loader's demand rounds and fetch pool

## 2. Backend Contract

### 2.1 Trait

In `event-broker-sdk/src/backend/`, bare names, no serde, no `SecurityContext`: backends are plugins linked into the gear, and one that reaches a remote store does so with its own credentials.

```rust
pub struct TopicPartition {
    pub topic: GtsInstanceId,
    pub partition: u32,
}

#[async_trait]
pub trait Backend: Send + Sync {
    async fn provision(&self, topic: &TopicSpec, claim: &LogClaim) -> Result<Provisioning, BackendError>;

    async fn append(&self, request: &AppendRequest<'_>) -> Result<Appended, BackendError>;

    /// What deduplication this backend guarantees; declared once and logged at startup.
    fn dedup(&self) -> Dedup;

    /// The most events one `append` may carry; ingest never exceeds it.
    fn max_events_per_append(&self) -> usize;

    async fn read(&self, at: &TopicPartition, offset: Sequence, max_events: usize, wait: Wait) -> Result<Vec<Event>, BackendError>;

    async fn resolve(&self, at: &TopicPartition, position: Position) -> Result<Sequence, BackendError>;

    fn watch(&self) -> Watch<'_>;

    async fn query(&self, at: &TopicPartition, range: PartitionRange) -> Result<Vec<TopicSegment>, BackendError>;

}
```

`Event`, `Position`, `PartitionRange` and `TopicSegment` are the SDK's existing types. `topic` is typed everywhere; no operation takes a topic as `&str`.

### 2.2 Append

```rust
/// Fencing token of one ingest outbox message: strictly increasing per
/// (outbox, partition), gaps allowed (feature 0005 §2.5).
pub struct Fence {
    pub outbox: OutboxId,            // event_broker__outbox_identity.outbox_id
    pub partition: i64,              // OutboxMessage.partition_id, as delivered
    pub seq: OutboxSeq,              // OutboxMessage.seq
}
pub struct OutboxId(u64);           // 16 hex in table names and logs
pub struct OutboxSeq(i64);           // a separate space from the backend's `Sequence`

pub struct AppendRequest<'a> {
    pub fence: Fence,
    pub topic: &'a GtsInstanceId,
    pub events: &'a [KeyedEvent],    // at most `max_events_per_append()`
    pub flush: Flush,
}

/// One event and the key the backend partitions it by.
pub struct KeyedEvent {
    /// The value at the event type's partition-key pointer (ADR-0002), resolved by ingest.
    pub key: String,
    pub event: Event,
}

/// When to return. Either way `append` returns only after every event is durably stored.
pub enum Flush {
    /// Return only once positions are assigned and readable; the answer is `Heads`.
    Sync,
    /// Do not wait for assignment or visibility. A plugin may treat this as `Sync`.
    Async,
}

/// Which one comes back is the implementation's choice under `Flush::Async`, and may differ per call.
pub enum Appended {
    /// The highest sequence per partition this call touched.
    Heads(Vec<Head>),
    /// Stored; positions become known through `watch`.
    Accepted,
    /// Already stored under this or a later fence of the same ingest partition; nothing written.
    Duplicate,
}

pub enum Dedup {
    /// The events and the last applied seq are written atomically; a fence at or below it is
    /// `Duplicate`. SQL databases, Kafka (transaction + fence partition), KurrentDB, JetStream.
    Fenced,
    /// Each target stream drops fences at or below the highest it has seen from this ingest
    /// partition; a partially stored message is completed by resending it whole. RabbitMQ Streams, Pulsar.
    ProducerSequence,
    /// One create-once object per fence. Object stores, filesystem.
    CreateOnce,
    /// No deduplication: a retried message may be stored twice. Kinesis, Pub/Sub.
    AtLeastOnce,
}

pub struct Head {
    pub at: TopicPartition,
    pub sequence: Sequence,
}
```

- **Partition and sequence are the backend's.** It maps `key` to one of the topic's partitions (§2.6) with a function of its own choice. The one rule: for a topic on a backend the mapping is deterministic - a key maps to the same partition on every instance and every call (ADR-0002). A changed function, re-partitioning, or a move between backends with different functions shifts per-key ordering onto the end user.
- **Sequences.** Per partition, an `i64` from 1 (ADR-0001); the partition set is fixed for the life of a provisioned topic. Events of one key keep their order within one call and across calls made in order.
- **Concurrent callers.** `append` is safe under concurrent calls from many processes for the same partition: one ingest partition carries many keys and topics, and several ingest partitions carry keys of one backend partition. Sequence assignment is atomic on the medium (row lock, atomic counter, the medium's own sequencer).
- **Visibility follows sequence order.** Nothing becomes readable below a sequence already readable. A plugin whose medium can commit out of order keeps a per-partition commit horizon and serves reads only up to it.
- **Durable first.** `Ok` means the events survive a crash of the gear and of the plugin, whichever `Flush` was asked for. A plugin that buffers writes does not return before its buffer is flushed to the medium.
- **Dropping the call.** The outbox drops an `append` future at its lease's cancel point (`libs/toolkit-db/src/outbox/handler.rs:56-57`). Dropping it leaves nothing that commits later, except a commit already in flight on the medium.
- **Exactly once per `Fence`** (unless the backend declares `AtLeastOnce`). An `append` whose fence is at or below the last one applied for its `(outbox, partition)` stores nothing and returns `Duplicate`, whichever copy commits second: a retry after an ambiguous failure, or a late commit of a dropped call.
- **Ordered per ingest partition.** Ingest sends a fence only after every lower `seq` of that ingest partition returned `Ok`; it never pipelines. The deduplication relies on it: a message a later one jumped over would be dropped as a duplicate on its retry and lost.
- **Strategy is the plugin's.** `Dedup` names how the backend keeps the guarantee, at a granularity the medium chooses - per `(outbox, partition)`, or per `(outbox, partition, stream)`, since a subsequence of a strictly increasing sequence is strictly increasing. The last applied `seq` is a detail of `Fenced`, not part of the trait.
- **Atomic or resendable.** `Fenced` and `CreateOnce` store a message's events all or nothing. `ProducerSequence` may store part of a message on a failure; resending the whole message completes it without duplicating the stored part.
- **Fence state lives as long as the data.** A plugin keeps its fence state (a table, a fence partition, a fence subject, a producer's tracking) under the same lifetime as the topics it guards: retention, compaction or a stream recreation that removes it re-admits old messages.
- **No per-event positions.** Ingest promises a producer no sequence, and nothing downstream needs one per event.

### 2.3 Read

```rust
pub enum Wait {
    /// Return what is stored now, possibly nothing.
    No,
    /// Return as soon as anything above `offset` is stored, or empty after this long.
    For(Duration),
}
```

- Events of `at` with sequence greater than `offset`, ascending, at most `max_events`. Every read is exclusive of the offset it names (ADR-0001): `offset` is the last position the caller has consumed, and `Sequence::NONE` reads from the start.
- `Wait::No` serves the loader: random reads, catch-up, history, and the tail once woken (§3.3).
- `Wait::For` is for callers that own a dedicated wait, never for a fetch holding a shared pool permit. A backend that can wait natively does so (PG `LISTEN`, a Kafka fetch with `fetch.max.wait.ms`, a JetStream pull with `expires`); one that cannot may return at once.
- Any read that returns empty is retried with the caller's backoff (`event-broker/src/infra/loader/poll.rs`), whatever the head cache says: a head can run ahead of what is readable on an `Accepted` backend.

### 2.4 Resolve

`Exact`, `Earliest`, `Latest`, `At(t)` to the offset a cursor may hold, with the meaning the SDK's `Position` documents. `Latest` is the partition's head - the highest sequence ever assigned - and is what a polled watch asks.

### 2.5 Watch

```rust
pub enum Watch<'a> {
    /// The medium pushes head changes.
    Native(&'a dyn HeadWatch),
    /// The gear polls `resolve(Latest)` for each watched partition at this interval.
    Poll(Duration),
}

pub trait HeadWatch: Send + Sync {
    /// First the current head of each named partition, then every change. May coalesce:
    /// only the latest head per partition matters. Ends when dropped. Errors arrive as
    /// stream items; the subscriber resubscribes with backoff.
    fn watch(&self, partitions: &[TopicPartition]) -> BoxStream<'static, Result<Head, BackendError>>;
}
```

- Per `(topic, partition)`, as sequences are: a subscriber names the partitions it cares about and hears nothing about the rest.
- The current head comes first, so a subscriber opened after an append still learns the head that append assigned.
- `Poll` keeps backends that cannot push inside the same interface: the gear turns the interval and `resolve(Latest)` into the same stream of `Head`s, current head first.

### 2.6 Provision and Query

```rust
pub struct TopicSpec {
    pub topic: GtsInstanceId,
    /// From the topic's resolved settings; the backend partitions keys into this many.
    pub partitions: u32,
    /// From the topic's resolved settings; the backend enforces it on its own schedule.
    pub retention: Retention,
}

pub struct Retention {
    pub max_age: Option<Duration>,   // events older than this are removed
    pub max_bytes: Option<u64>,      // per partition; the oldest events go first
}

pub struct LogClaim {
    pub replacing: Replacing,
    pub new: LogId,
}

pub enum Replacing {
    /// No earlier attempt is being replaced.
    Nothing,
    /// The attempt being replaced.
    Log(LogId),
}

pub enum Provisioning {
    /// The log exists and accepts writes; `partitions` is the count the backend actually holds.
    Ready { partitions: u32 },
    Pending,
}
```

- `provision` as feature 0006 §2.3. Compare-and-set, for either `Replacing` variant: accepted when nothing is stored for the topic, when the stored id is the one `Log` names, or when it is already `new`; any other stored id is `LogConflict`.
- The partition count travels with the topic, and `Ready` reports the count the backend holds - a pre-existing log, a Kafka topic created elsewhere, may hold another. The binding records it (feature 0006 §2.2), and ingest, the head pump and delivery use the recorded count. A configured count that differs from the recorded one does not take effect: the recorded count stays in force and the mismatch is logged at every provisioning check; changing a provisioned topic's count is out of scope (§7).
- **The bounds travel with the topic.** `provision` hands the backend the topic's retention; when the resolved settings change, the provisioning worker calls `provision` again with the new `TopicSpec` and the same log id (`LogClaim { replacing: Log(L), new: L }`, which the compare-and-set accepts as the same attempt continued), and the backend adopts the new bounds (feature 0006 §3.1).
- **The backend enforces retention itself.** It removes expired events on its own schedule, with its own workers; the gear runs no retention job and never calls into a backend to sweep. A backend that runs in several processes (every ingest and delivery process builds every registry entry) coordinates its workers on its own medium.
- `query` keeps its existing meaning.

### 2.7 Provider

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    /// The GTS backend type this provider builds, derived from `gts.cf.core.events.backend.v1~`.
    fn backend_type(&self) -> &GtsTypeId;

    /// Builds one registry entry (feature 0005 §2.1) from the settings beside its `type`.
    /// The backend's own workers (retention, storage upkeep) run until `stop` is cancelled.
    async fn build(&self, settings: &serde_json::Map<String, serde_json::Value>, stop: CancellationToken)
        -> Result<Arc<dyn Backend>, BackendError>;
}
```

- `build` validates the settings, including how the plugin talks to its medium, and connects lazily: an unreachable medium is a failed operation per topic (`Unavailable`) and a degraded readiness signal, never a failed start (feature 0005 C2).
- **Workers.** Whatever the backend runs in the background is started by `build` and stopped by `stop`: on cancellation each worker finishes its current pass and exits, bounded by a timeout.
- A `build` error names the setting and the rule it breaks. It never carries a setting's value or a deserializer's message quoting one, since settings hold expanded `${VAR}` secrets.

### 2.8 Errors

`BackendError` is the SDK's `StorageBackendError` plus:

| Variant | Raised by | Meaning |
|---|---|---|
| `LogConflict` | `provision`, `append` | the backend holds a log id the claim or the call does not accept (feature 0006) |
| `NotProvisioned` | `append`, `read`, `resolve`; a `HeadWatch` stream item | the topic has not been provisioned on this backend |

Messages name the operation and the rule broken, never a stored value or event content.

## 3. Processes / Business Logic (CDSL)

Pseudo-code below is Python-ish - not runnable; intent over syntax.

### 3.1 Ingest Write Path

```python
# publish (single event or batch; the producer protocol checks run first - ADR-0004)
for event in batch:
    key = resolve_pointer(event, event_type.partition_key)       # ADR-0002; no partition here
    group = outbox_group(topic)                                   # feature 0005 §2.5
    ip = murmur3(f"{topic}:{key}") % group.partitions            # ingest partition
wait = prefer_wait(request)                                       # Prefer: wait=N, capped by publish.max_wait (default 30s)
groups = outbox groups the batch touches                          # feature 0005 §2.5
if wait:
    for g in groups:
        trace[g] = Uuid::new_v4()                                 # broker-minted, never on the wire
        sub[g] = outbox[g].subscribe(trace[g])                    # before the enqueue commits
BEGIN                                                             # ONE ingest DB transaction, all or nothing
    producer chain checks and updates (ADR-0004)
    for g in groups: outbox[g].enqueue(&txn, events of g, trace[g])
COMMIT
if wait:
    outcomes = await all(sub[g] for g in groups) up to wait       # one future per outbox, joined
    return stored if every outcome complete  # 201
         | accepted if timed out             # 202: accepted, not yet confirmed
return accepted                                                   # 202; 200 when every event was a duplicate

# outbox handler
flush = Flush.Sync if payload.stored_wait else Flush.Async
fence = Fence(group.outbox_id, msg.partition_id, msg.seq)
match backend.append(AppendRequest(fence, topic, [KeyedEvent(key, event)], flush)):
    Ok(Heads(heads))      -> head_cache.publish(heads); Ok
    Ok(Accepted)          -> head_pump.follow(topic); Ok          # heads arrive through watch
    Ok(Duplicate)         -> Ok                                   # already stored; ack the outbox
    Err(NotProvisioned | LogConflict) -> Retry                    # endless, MVP; the ingest partition stalls
    Err(any other)        -> Retry                                # never dead-lettered; alerted
```

- **Outbox message.** One event per message (`payload_type` `evbk.ingest.v2`); batching is the outbox's own (its leased batch handler), not the payload's. The fence's `partition` and `seq` come from `OutboxMessage` as delivered.

  ```rust
  struct IngestPayload {
      topic: GtsInstanceId,          // resolved at publish
      key: String,                   // partition key value (ADR-0002) -> KeyedEvent.key
      stored_wait: Option<Uuid>,     // Prefer: wait trace -> Flush::Sync; None -> Flush::Async
      event: Event,                  // no partition, no sequence
  }
  ```
- **Outbox groups.** Each topic drains through its outbox group (feature 0005 §2.5), a toolkit-db queue with its own partitions; the ingest partition is `murmur3("{topic}:{key}") % group.partitions`: events of one key reach the backend in publish order, and the backend maps that key to one partition. Ingest never knows the backend partition.
- **Head-of-line blocking stays inside a group.** A backend answering `Retry` stalls the topics sharing its ingest partitions in that group; a topic that needs isolation gets its own group.
- **`NotProvisioned` and `LogConflict` at append retry endlessly** for the MVP. They mean a route change for a provisioned topic, or a backend that lost the topic; the operator reaction is ADR-0008's.
- **No dead-lettering.** Every `append` error is retried, endlessly; nothing an ingest outbox holds is dead-lettered or dropped. A message that keeps failing stalls its outbox partition, which the alerts below surface: retries per registry entry and outbox partition (`evbk_ingest_append_retries_total{backend, outbox, partition, error}`), and the age of the oldest unacked message per outbox partition (`evbk_ingest_outbox_oldest_age_seconds`), alerted past a threshold.
- **Declared dedup.** Ingest logs every registry entry's `dedup()` at startup, `AtLeastOnce` as a warning.
- **Stored publish.** `Prefer: wait=N` (RFC 7240) asks for an answer once the batch is stored. The trace rides in EB's own outbox payload, since `OutboxMessage` does not carry the trace it was enqueued under (`libs/toolkit-db/src/outbox/handler.rs:12`). A timeout answers `202`, which is retry-safe for chained and monotonic producers. No answer carries a sequence.

### 3.2 Head Cache

```python
# key:   head/{topic_uuid}/{partition}      topic_uuid = GtsId::to_uuid, as notify.rs
# value: the head sequence

head_cache.publish(heads):
    for h in heads:
        loop:
            (stored, version) = get(key(h.at))
            if stored >= h.sequence: break
            if compare_and_swap(key(h.at), version, h.sequence): break

head_pump.follow(topic):                     # at most one per topic in the cluster
    hold the pump lease for topic, or return
    open watch over every partition of topic (recorded partition count)
    for head in stream: head_cache.publish([head])
    on stream error: resubscribe with backoff
    every reconcile interval: publish resolve(tp, Latest) for each partition
    release after the topic has had no Accepted append for a fixed idle period
```

- **Replaces the payload-free `notif/...` wake-up** (`event-broker/src/domain/notify.rs`): a woken reader knows whether there is anything above its position before it reads.
- **Monotonic.** Concurrent writers never move a head backwards: the write is a compare-and-swap on the entry version (`ClusterCacheV1::compare_and_swap`).
- **One pump per topic.** The pump is held by one ingest instance through a cluster lease, so a topic's partitions are watched once, not once per instance.
- **Published after storage.** A head from `Heads` is published only after the events it covers are stored, so a woken read never races the write it was woken for.
- **A hint, not the authority.** A lost or late entry costs a late wake, and the low-rate reconcile repairs it. `resolve(Latest)` is the authority.

### 3.3 Delivery Reads

```python
# the loader runs demand rounds (infra/loader/scheduler.rs); every fetch takes a pool permit
for demand in round:
    absorb(backend.read(demand.tp, demand.after, n, Wait.No))     # never Wait.For under a permit
    if empty: backoff(demand.tp)                                  # loader/poll.rs

# tail: a partition whose readers sit at the frontier produces no demand until woken
on head_cache change(tp, head):
    if head > newest_accounted(tp): demand(tp, after = newest_accounted(tp))

# not provisioned yet (registered topic, binding not provisioned)
read / resolve -> NotProvisioned  =>  an empty partition: head 0, cursor range [0, 0]
```

- **No permit held across a wait.** A quiet tail never holds a fetch permit: its readers wait on the head cache, and the loader fetches with `Wait::No` once woken. Backfill, catch-up and other tails keep their share of the pool.
- **Unprovisioned topics read as empty.** A topic registered but not provisioned behaves like a partition nothing has been written to (DESIGN "Offset Semantics"): head 0, the only valid cursor 0, SEEK resolving to 0. Consumers may join before producers publish.
- **Own watch allowed.** Delivery may open a `watch` of its own over the partitions its readers hold instead of the cache; the cache is the default.

## 4. Meeting the Contract on a Medium

What a medium must offer for a plugin over it to satisfy §2, and the shape each answer takes. Each plugin's own documentation states which shape it is and how it meets it; this feature names no plugin.

Every plugin, whatever its medium:

- **Sequences:** per partition an `i64` from 1 (ADR-0001); a medium whose native positions start elsewhere, or are not `i64`, translates them with a fixed mapping or keeps a counter of its own.
- **Partitions:** a fixed set for a provisioned topic.
- **Key mapping:** deterministic per topic on a backend (§2.2).
- **Deduplication:** one `Dedup` strategy, with fence state kept as long as the data it guards.
- **Visibility and durability:** readable in sequence order, durable before `Ok`.

| Medium shape | `append` returns | `watch` | `dedup()` |
|---|---|---|---|
| Transactional store: events, counters and fence state commit together | `Heads` | `Poll`, or `Native` where the store notifies | `Fenced`: the fence advances in the transaction that stores the events |
| Partitioned log with transactions across its partitions | `Heads` | `Native` | `Fenced`: a fence record committed with the events |
| Log with per-producer idempotence, no multi-record transactions | `Heads` or `Accepted` | `Native` | `ProducerSequence`: one producer identity per (outbox, ingest partition) per target stream |
| Create-once object or file store | `Heads` or `Accepted` | `Poll` | `CreateOnce`: one object per fence, created only if absent |
| None of the above | as it can | as it can | `AtLeastOnce` |

## 5. Actor Flows (CDSL)

### 5.1 Stored Publish on a Backend That Returns Heads

```text
# orders on main (a backend that returns `Heads`), 4 partitions; producer publishes 3 events with Prefer: wait=10
ingest-1  trace T minted, subscribe(T); enqueue e0 (k=a), e1 (k=b), e2 (k=a) in one tx
handler   ip of a: append(orders, [k=a], Sync) -> p1 gets 41 -> Heads[(p1, 41)]
          ip of b: append(orders, [k=b], Sync) -> p3 gets 17 -> Heads[(p3, 17)]
          ip of a: append(orders, [k=a], Sync) -> p1 gets 42 -> Heads[(p1, 42)]
ingest-1  head/orders/1 = 42, head/orders/3 = 17
          outcome(T): 3 entities, 0 failures -> 201
```

### 5.2 Publish on a Backend That Assigns Later

```text
# audit on audit (a backend that returns `Accepted`), 2 partitions; no Prefer
ingest-2  enqueue -> 202 at once
handler   append(audit, [k=t9], Async) -> stored -> Accepted
ingest-2  head_pump.follow(audit): lease taken; watch([audit/0, audit/1]) -> current heads first
medium    audit/1 head 8812 -> head/audit/1 = 8812
```

### 5.3 A Reader at the Tail

```text
# delivery-1 reader on orders/1 at offset 42, cache says head 42
loader    no demand for orders/1; reader waits on head/orders/1
ingest    append -> Heads[(orders/1, 43)] -> head/orders/1 = 43
loader    demand(orders/1, after 42) -> read(orders/1, 42, 500, No) -> [43] -> absorbed, reader woken
```

## 6. Open Items

- **Ingest database restored from backup.** A restore brings back an `outbox_id` with a lower `seq` than the backends already applied, so new messages under it are dropped as duplicates. Rotating the identity on restore, or raising `seq` above every backend's applied position at startup, is to be decided.
- **`AtLeastOnce` backends.** Accepted with a startup warning, or refused at `build`, is to be decided.

## 7. Out of Scope

- Changing a topic's partition count after it is provisioned.
- Transactions and atomic appends across partitions.
- Moving a topic between backends (ADR-0008).
- Per-event positions in the append acknowledgement.

## 8. Definitions of Done

- `event-broker-sdk` exposes `backend::{Backend, Provider, BackendError}` and the types in §2; the trait `EventBrokerBackend` and `EventBrokerBackendProvider` are removed.
- Every storage backend plugin in the workspace implements the trait and declares its `Dedup`; none deduplicates on producer chain numbers. `redelivery_after_persist_is_a_safe_noop` (`event-broker/src/infra/workers/ingest_outbox.rs:294-331`) asserts `Duplicate`.
- Ingest resolves the partition key, enqueues into its outbox group (feature 0005 §2.5) by `murmur3("{topic}:{key}") % group.partitions`, stamps no partition, and appends with `Flush` per §3.1; `Prefer: wait` answers per §3.1.
- The head cache replaces `notif/...`; writes are compare-and-swap; one pump per topic runs per §3.2.
- Delivery's loader reads with `offset: Sequence` and `Wait::No` per §3.3, and maps `NotProvisioned` to an empty partition.
- The ingest database migrations run on SQLite, PostgreSQL and MySQL/MariaDB.

## 9. Acceptance Criteria

- **AC-1**: Events of one key published in order are read back from one partition in that order.
- **AC-2**: A publish with `Prefer: wait` answered `201` has its events readable through `read`.
- **AC-3**: `append` returning `Ok` survives an immediate crash of the gear, for `Flush::Sync` and `Flush::Async` alike.
- **AC-4**: A backend answering `Accepted` has its heads reach the head cache through `watch`, native or polled, including a head assigned before the watch opened.
- **AC-5**: A reader at the tail receives a new event without a poll of the backend between heads, and holds no fetch permit while it waits.
- **AC-6**: `read` never returns the event at `offset`, only those above it.
- **AC-7**: Concurrent `append` calls for one partition from two processes assign distinct, increasing sequences, and a reader never sees a sequence appear below one it has read.
- **AC-8**: A registered topic that is not provisioned reads as an empty partition.
- **AC-9**: A retry of an applied message returns `Duplicate` and stores nothing, including after a hard restart of ingest and of the backend plugin.
- **AC-10**: A late commit of a dropped call, arriving after its retry and a later message, stores nothing.

## 10. Unit Test Plan

- **append** (per plugin): keys spread over partitions; order kept per key; a key maps to the same partition across calls and instances; `Heads` names every partition touched with its highest sequence; first sequence of a fresh partition is 1; concurrent appends to one partition assign distinct sequences; `Sync` returns `Heads` and the events are readable.
- **read**: exclusive of `offset`; `Sequence::NONE` from the start; `max_events` respected; `Wait::For` returns on a concurrent append and returns empty after its duration.
- **watch**: the current head comes first; `Native` yields heads only for the named partitions; `Poll` adapter yields a head per change of `resolve(Latest)` and none without one; an error item leads to a resubscription.
- **head cache**: a lower head never overwrites a higher one under concurrent writers; one pump per topic across two instances.
- **ingest handler**: stored-wait payload -> `Flush::Sync`; otherwise `Flush::Async`; `Accepted` starts the head pump; `NotProvisioned` and `LogConflict` -> `Retry`.
- **ingest partition**: `murmur3("{topic}:{key}") % group.partitions` is stable for a given topic and key.
- **dedup** (per plugin): plain retry -> `Duplicate`; a late copy after a later fence -> nothing stored; gaps in `seq` accepted; two `outbox_id`s on one backend kept apart; state survives a restart.

## 11. E2E Test Plan

Suite: `testing/e2e/suites/event_broker/`.

- **S1 - key order**: publish interleaved events for two keys; consume; assert each key's events arrive in publish order from one partition.
- **S2 - stored publish**: publish with `Prefer: wait=10`; assert `201` arrives only after the events are readable, and carries no sequence.
- **S3 - tail wake**: open a stream at the tail; publish; assert delivery within the heartbeat interval.
- **S4 - crash after append**: kill the gear right after a publish is answered `201`; restart; assert the events are read back.
- **S5 - consumer before producer**: register a topic; join and seek earliest before any publish; assert the join succeeds, then the first published event is delivered.
- **S6 - crash between commit and ack**: kill ingest right after a backend commit, before the outbox ack; restart; assert the event is stored once.
