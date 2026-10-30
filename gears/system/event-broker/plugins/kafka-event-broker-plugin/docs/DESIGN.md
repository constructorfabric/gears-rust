# Technical Design - Kafka Event Broker Plugin

- [1. Settings](#1-settings)
- [2. Topic Layout and Naming](#2-topic-layout-and-naming)
  - [2.1 Names](#21-names)
  - [2.2 Event Records](#22-event-records)
  - [2.3 Logs and Fences Records](#23-logs-and-fences-records)
- [3. Clients and Fencing](#3-clients-and-fencing)
  - [3.1 Clients](#31-clients)
  - [3.2 Fencing Design](#32-fencing-design)
  - [3.3 Compacted-Topic Copies](#33-compacted-topic-copies)
- [4. Sequences, Offsets and Gaps](#4-sequences-offsets-and-gaps)
- [5. Operations](#5-operations)
  - [5.1 provision](#51-provision)
  - [5.2 append](#52-append)
  - [5.3 read](#53-read)
  - [5.4 resolve](#54-resolve)
  - [5.5 query](#55-query)
  - [5.6 watch](#56-watch)
  - [5.7 start and stop](#57-start-and-stop)
- [6. Retention Granularity](#6-retention-granularity)
- [7. Error Mapping](#7-error-mapping)
- [8. Broker Prerequisites](#8-broker-prerequisites)
- [9. Mirroring (MirrorMaker)](#9-mirroring-mirrormaker)
- [10. Limits and Known Limitations](#10-limits-and-known-limitations)
- [11. Later](#11-later)

The plugin implements `event_broker_sdk::backend::Backend` over an Apache Kafka cluster for the backend type `gts.cf.core.events.backend.v1~cf.core.backend.kafka.v1~`. Each event-broker topic is one Kafka topic, each partition one Kafka partition. Deduplication is `Dedup::Fenced`: the events of an append and its fence record commit in one Kafka transaction of a producer owned by the fence key. Heads are polled (`Watch::Poll`). Retention is the brokers' own.

The client is `rdkafka` 0.38 over a vendored librdkafka 2.10 (`rdkafka-sys` 4.9, pinned in the workspace `Cargo.toml`), with the `tokio` runtime. The plugin is first-party and always linked into the gear.

## 1. Settings

Settings are read by hand (`src/settings.rs`): an unknown setting or a broken rule is `InvalidConfig` naming the setting and the rule, never the value. `build` validates and connects nothing.

| Setting | Default | Meaning |
|---|---|---|
| `bootstrap_servers` | required | `host:port[,host:port...]`; `${VAR}` is expanded. Redacted in `Debug`. |
| `topic_prefix` | `evbk` | 1 to 200 characters of `[a-zA-Z0-9._-]`. Every Kafka topic and transactional id of the entry starts with it, so two prefixes on one cluster are two backends that share nothing. |
| `replication_factor` | `3` | Replicas of every topic the plugin creates, 1 to 32. |
| `min_insync_replicas` | `2` | `min.insync.replicas` of every topic the plugin creates; at most `replication_factor`. |
| `fence_partitions` | `16` | Partitions of the fences topic **when this instance creates it**. Every instance takes the count from the cluster's metadata of the existing topic (§3.2), so a value that differs from the topic's - lowered, raised, or another instance's - changes nothing but a warning at first use. |
| `poll_interval_ms` | `500` | The `Watch::Poll` interval, and how often a waiting read probes again (§5.3). |
| `consumer_pool` | `8` | Most consumers one instance holds at once (§3.1). |
| `producer_pool` | `16` | Most fence keys one instance holds a slot and a transactional producer for, at most 256 (§3.2). Each producer costs librdkafka threads and a connection per broker (§3.1). |
| `request_timeout_ms` | `10000` | Bound of one cluster request: admin calls, offsets lookups, transaction steps, a catch-up (§3.3). Also `socket.timeout.ms`, so at most `transaction_timeout_ms`: librdkafka refuses to create a producer whose socket timeout exceeds its transaction timeout. |
| `transaction_timeout_ms` | `10000` | `transaction.timeout.ms` (and `message.timeout.ms`) of the plugin's producers: how long a transaction abandoned by a crashed process holds back `read_committed` readers before the coordinator aborts it. At most the broker's `transaction.max.timeout.ms`. |
| `client` | `{}` | librdkafka properties passed to every client, from an allow-list: connection, socket (not `socket.timeout.ms`, which is `request_timeout_ms`), retry and backoff, metadata refresh, batching and fetch sizes, compression, `client.id`, `debug`, `statistics.interval.ms`. Everything that decides the plugin's guarantees (`acks`, idempotence, `transactional.id`, `isolation.level`, offsets, groups, `bootstrap.servers`) and every security property is refused. |

The role (`Ingest`, `Delivery`, `Standalone`) changes nothing: the plugin has no background work to place.

## 2. Topic Layout and Naming

### 2.1 Names

| Kafka object | Name | Configuration |
|---|---|---|
| One event-broker topic's log | `<prefix>.<topic uuid, 32 hex>` | `cleanup.policy=delete`, `message.timestamp.type=LogAppendTime`, `retention.ms`, `retention.bytes`, `segment.ms` (§6), `min.insync.replicas`; partitions and replication from the spec and settings |
| Log records (§2.3) | `<prefix>.logs` | 1 partition, `cleanup.policy=compact` |
| Fence records (§2.3) | `<prefix>.fences` | `fence_partitions` partitions, `cleanup.policy=compact` |
| Provisioning lock of one topic | transactional id `<prefix>.log.<topic uuid>` | - |
| One fence key's producer | transactional id `<prefix>.fence.<fence key, 32 hex>` | - |

The topic uuid is `support::topic_uuid` of the GTS topic id. The plugin creates the logs and fences topics on an instance's first need (`TopicAlreadyExists` is success) and takes an existing one as it is. The plugin's consumers carry the group id `<prefix>.reader` but never join it, commit or store an offset: every read assigns its partition and offset itself.

### 2.2 Event Records

One event is one record in its partition (`support::partition_for` of the event's key, as every backend chooses):

- key: the event's partition key, bytes as given;
- value: versioned JSON of every event field but the positions and `meta` (`src/body.rs`, the same shape as the PostgreSQL plugin's body); an unknown version is `Internal` on read;
- timestamp: the broker's `LogAppendTime`, in milliseconds - the event's `sequence_time`. The producer's `occurred_at` stays in the value and never reaches the timestamp.

### 2.3 Logs and Fences Records

| Topic | Key | Value |
|---|---|---|
| logs | the topic uuid as 32 hex characters | JSON `{"v":1,"topic":<GTS id>,"log_id":<uuid>,"partitions":<n>}` |
| fences | the fence key's 16 bytes, big-endian | 9 bytes: version `1`, then the last applied fence seq as a big-endian `i64` |

A fence key's records all go to partition `key mod fence_partitions`, chosen by the plugin, not the client's partitioner. A value of another version is read as absent.

## 3. Clients and Fencing

### 3.1 Clients

An instance holds, all created on first need (`src/clients.rs`, `src/shared.rs`):

- one admin client (topic creation, metadata, configs);
- one `read_uncommitted` consumer that answers only the true high watermark of a compacted-topic partition (§3.3), and one `read_committed` consumer that answers only last stable offsets after a commit (§5.2); neither is ever assigned anything, so every caller shares them;
- a pool of at most `consumer_pool` `read_committed` consumers. A read takes one, assigns one partition at an offset, and gives it back unassigned; a consumer that failed is dropped, not pooled;
- at most `producer_pool` transactional producers, one per recently used fence key (§3.2), plus a short-lived one per `provision` call (§5.1).

librdkafka clients run their own threads; destroying one flushes and joins them. Blocking librdkafka calls (`init_transactions`, `commit_transaction`, offsets lookups, metadata) may each block for a request timeout. The plugin runs both on **detached threads**, never on the async workers and never on the runtime's blocking pool: a runtime shutting down waits for every blocking-pool task, so a call still blocked there would hold up the process's exit. That includes the last reference of a producer whose call was dropped.

**Cost.** A librdkafka client runs a main thread, an internal one, and one thread and one connection per broker it talks to. With 3 brokers that is about 5 threads and 3 connections per producer, so the default `producer_pool` of 16 is up to about 80 threads and 48 connections for fence producers per instance, plus the consumers (up to `consumer_pool`) and the three single clients. Size `producer_pool` to the number of fence keys one instance appends under at once (ingest writers), not to the total.

### 3.2 Fencing Design

Exactly once per fence (`Dedup::Fenced`) rests on Kafka transactions, one transactional id per fence key (`src/fence.rs`, `src/append.rs`):

1. **Open.** The first append under a key in an instance creates the key's producer and calls `init_transactions`. The coordinator bumps the id's epoch, which fences every earlier producer of the id in any process, and settles the transaction such a producer left open (rolls a prepared commit forward, aborts anything else).
2. **Read the fence.** The plugin then reads the key's fences partition up to its true high watermark (§3.3): every fence any earlier producer of the key committed is in it. The last seq applied is kept with the producer.
3. **Compare.** An append whose seq is at or below it answers `Duplicate` and writes nothing.
4. **Write.** Otherwise one transaction carries the events and the key's new fence record, and commits. Only then is the kept seq raised.

While the producer is not fenced nobody else writes under its key, so the kept seq is the truth. When another instance opens the key, this producer's next transaction is refused by the broker (`ProducerFenced`, or librdkafka purging what it had queued); nothing of it becomes visible. The plugin drops the producer and opens it once more within the same call, which fences the other instance in turn and re-reads the fence: a late copy of a fence the other instance applied answers `Duplicate`. Two instances racing on one key make progress one call at a time; a second fencing within one call is `Unavailable`.

Any failure in steps 1 to 4 drops the producer. A commit whose outcome is unknown (a timeout) is settled by the next open, so the caller's retry of the same fence reads it either applied (`Duplicate`) or not (written again). Before dropping, the plugin aborts the open transaction where librdkafka still can, so it does not hold back readers until `transaction_timeout_ms`.

**The fences partition** of a key is `key mod n`, with `n` the fences topic's partition count read from the cluster's metadata once per instance, never the setting: every instance must find a key in the same partition, or one would read no fence there and apply a fence again, and a partition past the topic's would fail every append.

**A dropped call.** A caller that gives up mid-append drops the transaction's guard, which aborts the transaction and drops the producer on the blocking pool; the key's next append opens a new producer (steps 1 and 2). Nothing is left open to hold back readers until `transaction_timeout_ms`.

**The producer pool** holds at most `producer_pool` keys. A new key takes the place of the least recently used idle key, whose producer is closed and opened again on its next append (steps 1 and 2). When every key in the pool is busy, the new key's append waits until one is idle: the pool never grows past its capacity.

### 3.3 Compacted-Topic Copies

An instance keeps an in-memory copy of the logs topic and of each fences partition it has used (`src/tail.rs`): the latest value per key, and the next offset to read. A catch-up reads forward with a `read_committed` consumer until its position reaches the partition's **true** high watermark, taken from the `read_uncommitted` consumer at the start of the catch-up. A `read_committed` consumer alone would stop at the last stable offset, before a transaction still open - possibly one of another key that committed after it - so the catch-up waits until every transaction below the watermark is decided, at most `request_timeout_ms`, and is `Unavailable` past it. A catch-up that follows `init_transactions` of the transactional id that writes a key therefore sees every value of that key committed before it.

Compaction keeps these topics at one record per topic and per fence key.

## 4. Sequences, Offsets and Gaps

An event's sequence is its record's offset plus one: offsets start at 0, sequences at 1 (`src/names.rs`). A cursor `s` reads offsets `>= s`.

Every write is transactional, and each transaction leaves a commit (or abort) marker in every partition it touched. Markers occupy offsets and are never delivered, and an aborted transaction's records are skipped by `read_committed` readers. **Sequences therefore rise strictly but have gaps**: after a transaction appending events at offsets 0 and 1 the marker takes 2, and the next event is offset 3 - sequences 1, 2, 4. The contract allows gaps (feature 0007); the PostgreSQL, SQLite and MySQL plugins have none.

**A head is always the last event's sequence, never the high watermark.** The high watermark (and the last stable offset a `read_committed` reader sees) sits past the marker, one or more above the last event. A head above the last readable event would make a waiting reader (`LocalHeads::read_waiting`) find nothing and probe again in a loop. So:

- `append` reports, per touched partition, the highest offset the broker assigned to one of its events, plus one;
- `resolve(Latest)` finds the last committed record below the last stable offset by reading back (§5.4);
- `Watch::Poll` heads are `resolve(Latest)`.

`heads_are_the_last_event_never_the_watermark` pins this.

## 5. Operations

### 5.1 provision

`src/provision.rs`. A topic has 1 to 10000 partitions on this backend.

**Calls and attempts.** The work below runs as a task of the instance - a provisioning attempt - never in the caller's future. A call starts an attempt for its topic, or joins the one running for the same spec and claim, and waits at most 2 s for its answer; past that it answers **`Pending`** and the attempt goes on. A caller that gives up (the gear bounds startup provisioning at 10 s) cancels nothing half-done, and its next call picks the same attempt up. An attempt's answer is handed to one call, which ends the attempt; a call for another claim while an attempt runs answers `Pending` and starts its own once that one has ended. Attempts count as in flight for `stop` (§5.7).

**A cold coordinator.** A broker creates its transaction state log on the first `init_transactions` it ever serves, and until then - or while it still settles an earlier producer's transaction of the same id - answers `CoordinatorLoadInProgress`, `CoordinatorNotAvailable`, `NotCoordinator` or `ConcurrentTransactions`. Opening a producer retries those, and timeouts, with backoff from 100 ms doubling to 2 s: an attempt for up to 6 request timeouts, an append (§3.2) for one. A producer a failed attempt left fatal is replaced.

The attempt:

1. Create the logs and fences topics if this instance has not yet.
2. Open a producer with the topic's transactional id `<prefix>.log.<uuid>` (`init_transactions`): it fences every other provisioning of the topic until this call ends, so the compare-and-set below is one at a time per topic across processes.
3. Catch up the logs topic (§3.3) and read the topic's record. If the claim does not accept its log id, `LogConflict`.
4. Look the Kafka topic up in the cluster metadata.
   - Missing: create it (`CreateTopics` with the §2.1 configuration; `TopicAlreadyExists` is success) and answer **`Pending`** without claiming the log: the topic is not provisioned until a later call finds it ready. The gear calls again with the same claim.
   - A partition without a leader: `Pending`.
5. Bring the topic's configuration to the spec's if any owned setting differs (§6). `AlterConfigs` is the non-incremental one librdkafka's Rust binding offers, so it carries the whole owned set and resets any other topic setting to the broker default.
6. Unless the stored record already says the same, write `{log id, partitions}` to the logs topic in a transaction of the producer from step 2, and commit. A producer fenced meanwhile by another provisioning cannot commit, so a lost race never overwrites the winner.
7. `Ready { partitions }` with the count the Kafka topic holds, which an existing topic may make differ from the spec's. The plugin does not add partitions.

An event-broker topic without a record is `NotProvisioned` for `append`, `read`, `resolve`, `query` and the watch, even when its Kafka topic exists. An instance remembers a provisioned topic forever (a topic is never removed); a topic it does not know costs a logs catch-up per call, one offsets request when nothing is new.

### 5.2 append

`src/append.rs`. At most 1000 events (`max_events_per_append`); more is `AppendFailed`. An append counts as in flight from its start, and `stop` waits for it (§5.7).

1. The topic must be provisioned (§5.1), before the fence is looked at: a fence that has passed its seq is no answer for a topic the backend does not hold.
2. Appends under one fence key run one at a time in an instance (the key's slot lock).
3. Fence and transaction as in §3.2: events to their partitions, the fence record to the key's fences partition, `acks=all`, idempotent, one transaction.
4. After the commit, wait until each touched partition's last stable offset has passed the append's head, asking the shared `read_committed` offsets consumer (no pooled consumer held) every 1 ms, doubling to 50 ms. The commit returns once the coordinator has decided; the brokers write the partitions' markers - which make the events readable - a moment later. So `append` returns only once its events are readable, `Flush::Async` included (the plugin treats it as `Sync` and always answers `Heads`). If they are not readable within `request_timeout_ms` the append still answers `Heads` (the events are committed) and logs a warning.
5. Publish the heads to the instance's `LocalHeads`, which wakes this instance's waiting reads.

The events are durable when the commit returns: `acks=all` with the topic's `min.insync.replicas`.

### 5.3 read

`src/read.rs`. With a pooled consumer: the partition's watermarks (`read_committed`, so the high one is the last stable offset); nothing past it means an empty answer at once. Otherwise assign the partition at `max(offset, low watermark)` and collect committed records until `max_events`, the last stable offset, or end-of-partition. A position below the log start (removed by retention) starts at the oldest stored record.

`Wait::For` is `LocalHeads::read_waiting`: probe, then sleep until this instance publishes a head past the offset or `poll_interval_ms` passes, until the wait is over. Appends through another instance are seen within one interval.

### 5.4 resolve

Bounds of a partition, from its watermarks `[low, stable)`:

- **ceiling** (`Latest`): the last committed record in `[low, stable)`, found by reading back from `stable` in windows of 4, 64, 1024, ... offsets, cached against `stable` (the same watermark needs no new read). A window not read to its end within `request_timeout_ms` is `Unavailable` and caches nothing: an unread window is no evidence the partition is empty. Never lowered within an instance, and never below a head this instance appended. When the partition holds no committed record: `0` if its log starts at offset 0 (nothing ever stored), otherwise `stable - 1` - every event removed by retention, and with every write transactional the record below the watermark is the last commit marker, so `stable - 1` is the last event's sequence when the last transaction committed;
- **floor** (`Earliest`): `min(low, ceiling)` - the cursor before the oldest stored record;
- `Exact(s)`: `s` when `floor <= s <= ceiling`, else `OffsetOutOfRange`;
- `At(t)`: `offsets_for_times` at `t` rounded up to the millisecond (record timestamps are milliseconds of `LogAppendTime`), giving the first record stored at or after `t`; the cursor is its offset, clamped to `[floor, ceiling]` (the record found may be a marker past the last event). Nothing stored at or after `t`: `ceiling`.

### 5.5 query

The matched range's first and last sequence and storage time, read from the partition (at most `limit` records), and one `segments` entry describing where the log lives: `kafka_topic`, `kafka_partition`, `log_start_offset`, `last_stable_offset`. Kafka keeps its segment files to itself, so there is nothing finer to report.

### 5.6 watch

`Watch::Poll(poll_interval_ms)`: Kafka pushes nothing a client could watch heads by. The gear polls `resolve(Latest)` per watched partition; while a head stands still that costs one offsets request (§5.4 cache), and a moved head one short read back.

### 5.7 start and stop

`start` does nothing: retention is the brokers' and clients are created on first need, so an unreachable cluster is `Unavailable` per operation, never a failed start. `stop` makes every later operation `Unavailable`, waits for the appends and provisioning attempts in flight to end - in good order until its deadline is cancelled, then at once - and then closes the consumer pool and the producers and drops every client. An append it stopped waiting for ends on its own; one not yet past its fence check answers `Unavailable`. Nothing it leaves behind blocks: client drops and blocking calls run on detached threads (§3.1), so with its deadline cancelled `stop` returns at once and the runtime shuts down without waiting for librdkafka. It touches no other instance.

## 6. Retention Granularity

Retention is the brokers' (`log.retention.check.interval.ms`, default 5 minutes). From the spec:

| Bound | Topic setting |
|---|---|
| `max_age` | `retention.ms` (unset: `-1`), and `segment.ms = min(max_age, 7 days)` |
| `max_bytes` | `retention.bytes` per partition (unset: `-1`) |

Both age by the broker's `LogAppendTime` - storage time - never by `occurred_at`. Kafka removes whole segments, oldest first, and only once a segment's newest record is past `retention.ms`, or while the partition without the segment is still at or over `retention.bytes`. A segment rolls at `segment.ms` or at the broker's `segment.bytes` (1 GiB default). So an event outlives its age bound by up to one segment span plus the check interval - about `2 x max_age` with the segment bound above, and less with a short `max_age` and steady traffic - and a partition may hold up to one segment (up to `segment.bytes`) more than `max_bytes`. The active segment is removed too once it has expired. A retention change is applied by the next `provision` (§5.1 step 5).

## 7. Error Mapping

`src/error.rs`. Messages carry the operation and librdkafka's error code name only; librdkafka's text may quote broker addresses and topic names.

| Failure | `BackendError` |
|---|---|
| Transport, resolve, timeouts, all brokers down, no leader / not leader / no coordinator, not enough replicas, concurrent transactions, fenced producer, purged queue, unknown topic or partition (a topic just created), authentication | `Unavailable` |
| Any other broker or client error code | `AppendFailed` in `append`, `ReadFailed` in `read`, `resolve`, `query`, `Internal` in `provision` |
| A client failure with no code (configuration, creation) | `Internal` |
| No log record for the topic | `NotProvisioned` |
| A partition index past the topic's count | `PartitionNotFound` |
| A catch-up past `request_timeout_ms` | `Unavailable` |
| Any call after `stop` | `Unavailable` |

## 8. Broker Prerequisites

- **Kafka 3.x or 4.x** with transactions available (tested on Apache Kafka 4.3.1, KRaft).
- **Topic auto-creation off** (`auto.create.topics.enable=false`). The plugin creates its topics itself; with auto-creation on, a read of a topic that does not exist yet could create it with broker defaults (wrong timestamp type, retention and replication).
- **The transaction state log** (`__transaction_state`) must be creatable and writable: `transaction.state.log.replication.factor` and `transaction.state.log.min.isr` no higher than the brokers available. On a single broker both must be 1, or every `init_transactions` times out.
- `transaction.max.timeout.ms` at least `transaction_timeout_ms`.
- The plugin's principal may create topics with the prefix, describe and alter their configs, and use transactional ids with the prefix (with ACLs: `CREATE`, `DESCRIBE_CONFIGS`, `ALTER_CONFIGS` on `<prefix>*` topics, `WRITE`/`DESCRIBE` on `<prefix>*` transactional ids, `IDEMPOTENT_WRITE` on the cluster). Only plaintext connections today (§11).
- `transactional.id.expiration.ms` (default 7 days) forgets an idle fence key's producer id; the fence itself stays in the fences topic, so nothing is lost when it does.
- Retention checks run every `log.retention.check.interval.ms`; the integration tests set it to 1 s.

## 9. Mirroring (MirrorMaker)

Per ADR 0008 a topic's log id is moved, never copied: at most one backend holds it.

- A topic's identity on this backend is its record in `<prefix>.logs`. Mirroring the event topic without its logs record leaves the destination `NotProvisioned`; mirroring the whole logs topic makes the destination hold every topic's log id. A whole-cluster mirror therefore must remove, on the destination, every logs record but the moved topics' (a tombstone per key), as ADR 0008 asks of any whole-medium copy.
- **MirrorMaker 2 does not preserve offsets**, and sequences are offsets plus one. Mirrored events get other sequences on the destination, and cursors held for the source do not carry over: a move by mirroring is a move with new positions, and consumers must seek (for one, `At` by storage time - which is also not preserved unless the mirror keeps `LogAppendTime`, which it does not: the destination broker stamps its own).
- Transactional state is not mirrored. Mirror the `<prefix>.fences` topic with the logs topic so a replayed fence stays a duplicate on the destination; a fence key's producer opened there starts a new epoch and reads the mirrored fence. A mirror that copies `read_uncommitted` would also copy aborted records as live ones; mirror `read_committed`.

## 10. Limits and Known Limitations

- At most 1000 events per append; at most 10000 partitions per topic.
- **Gaps** in sequences (§4); a per-event-count comparison of sequences is meaningless, as it is everywhere.
- **`sequence_time` has millisecond precision** (`LogAppendTime`), against microseconds on the SQL backends; `At` rounds up accordingly.
- **Latest after total removal.** A fresh instance answers `stable - 1` (§5.4); if the partition's last transaction aborted, that sits above the last event, and a reader holding the true last sequence sees one empty read before it is caught up.
- **The first append under a fence key in an instance** opens its producer and reads its fences partition: on the test broker about a second, against single-digit milliseconds for later appends. A process that cycles through more fence keys than `producer_pool` pays it again, and an append waits while every pooled key is busy (§3.2).
- **Every append is a transaction**: begin, produce, commit, then the wait for the markers (§5.2 step 4). A single-event append takes a few milliseconds even on a local broker; batches amortize it.
- **An abandoned transaction** (a crashed process) holds back `read_committed` readers of every partition it touched until the coordinator aborts it after `transaction_timeout_ms`, or until the key's producer is opened again.
- **Reads at the tail** cost an offsets request; a read that finds new records also an assign and a fetch. Single-event reads are therefore far slower than batched ones.
- **Head watching is polled** (§5.6): delivery sees another instance's append within `poll_interval_ms`.
- **Partitions are not added.** A topic provisioned again with more partitions keeps the count it has (`Ready` reports it).
- `AlterConfigs` is non-incremental (§5.1): a topic setting an operator set by hand outside the owned set (§2.1) is reset to the broker default on the next retention change. It works on the 4.3.1 test broker (`a_retention_change_reconfigures_the_topic`).
- The plugin keeps every fence key it has used and every logs record in memory per instance, one entry each.

## 11. Later

- **TLS and SASL.** Only plaintext is supported: `security.protocol`, `ssl.*` and `sasl.*` are refused by the `client` allow-list until they get settings of their own (secrets through `${VAR}` expansion, redacted like `bootstrap_servers`).
- Incremental config changes (`IncrementalAlterConfigs`) once the Rust binding exposes them.
- Adding partitions on a re-provision that asks for more.
- A native head watch (`Watch::Native`) from a tailing consumer, if polling proves too slow or costly.
