# Technical Design - SQLite Event Broker Plugin

- [1. Overview](#1-overview)
  - [1.1 Role](#11-role)
  - [1.2 Contract Coverage](#12-contract-coverage)
- [2. Domain Model](#2-domain-model)
  - [2.1 Tables](#21-tables)
  - [2.2 Column Types and Sizing](#22-column-types-and-sizing)
  - [2.3 Event Body](#23-event-body)
- [3. Component Model](#3-component-model)
  - [3.1 Startup and Connections](#31-startup-and-connections)
  - [3.2 The Per-File Hub](#32-the-per-file-hub)
  - [3.3 Migrations](#33-migrations)
- [4. Operations](#4-operations)
  - [4.1 provision](#41-provision)
  - [4.2 append](#42-append)
  - [4.3 Deduplication Cases](#43-deduplication-cases)
  - [4.4 read](#44-read)
  - [4.5 resolve](#45-resolve)
  - [4.6 watch](#46-watch)
  - [4.7 query](#47-query)
  - [4.8 Retention Sweep](#48-retention-sweep)
- [5. Configuration](#5-configuration)
- [6. Limits](#6-limits)
- [7. Error Mapping](#7-error-mapping)
- [8. Operations Notes](#8-operations-notes)
- [9. Risks / Trade-offs](#9-risks--trade-offs)
- [10. Later](#10-later)

## 1. Overview

### 1.1 Role

A storage backend for the event-broker gear over one SQLite database file. It implements `backend::Backend` and ships a `backend::Provider` for the backend type `gts.cf.core.events.backend.v1~cf.core.backend.sqlite.v1~` (event-broker feature 0007). One `backends` registry entry (feature 0005 §2.1) is one file this plugin owns.

It is the small-footprint backend: a single process, small volumes, no server to run. The design is deliberately simpler than the PostgreSQL plugin's - one event table, no segments, no partitioning of the table, no background maintenance besides retention.

**One process owns the file.** Several backend instances in that process may share it (§3.2); a second process opening the same file is unsupported: its appends are still correct (SQLite's file lock serializes writers and the sequences stay gapless), but heads it commits never reach this process's hub (§3.2): waiting reads see them only on their one-second re-read, watches not until this process next appends to the partition, and every instance in both processes runs its own sweep. The gear refuses a SQLite backend in every cluster mode at startup (event-broker feature 0005 §2.4, C10), so the one-process rule is enforced at configuration level there; a second `standalone` process on the same file remains unsupported and undetected.

References:

- event-broker `docs/features/0005-backend-registry-and-routing.md` - registry entries
- event-broker `docs/features/0006-topic-provisioning.md` - provisioning attempts, log id
- event-broker `docs/features/0007-storage-backend-api.md` - the trait this plugin implements
- event-broker `docs/ADR/0001-offset-semantics.md` - sequences from 1, cursor 0

### 1.2 Contract Coverage

| Contract item | This plugin |
|---|---|
| `append` result | `Heads` |
| `dedup()` | `Fenced`: the fence row and the events commit in one transaction |
| `watch()` | `Native`: the per-file hub (§3.2) |
| `provision` | always `Ready` in one transaction; never `Pending` |
| `max_events_per_append()` | 1000 |
| Key to partition | `(murmur3_x86_32(key, 0) & 0x7FFF_FFFF) % partitions` (`event_broker_sdk::backend::support::partition_for`) |
| Sequences | per partition, from 1, **gapless**: assigned under the file's single write lock |
| `Flush` | `Sync` and `Async` behave the same: every append commits before it returns |
| Durability | `synchronous=FULL` (default): every commit is synced to disk before `append` returns |
| Retention | per event, by `sequence_time` and by body bytes (§4.8) |

## 2. Domain Model

### 2.1 Tables

Defined in `src/migrations/0001_plugin_tables.sql`, which also lists the queries each table serves.

| Table | Key | Holds |
|---|---|---|
| `evbk__log` | `topic_uuid` | one row per provisioned topic: its id, log id (feature 0006), partition count, retention bounds (`max_age_ms`, `max_bytes`, NULL = unbounded) |
| `evbk__partition` | `(topic_uuid, partition)` | `hwm` (highest sequence ever assigned), `floor` (highest sequence removed), `bytes` (body bytes in `(floor, hwm]`), `head_time` (`sequence_time` at `hwm`) |
| `evbk__fence` | `fence_key` | `last_seq` applied under each fence key |
| `evbk__event` | `(topic_uuid, partition, sequence)` | every stored event: `sequence_time`, `tenant_id`, `size` (= `length(body)`), `body` |

Index `evbk__event_time (topic_uuid, partition, sequence_time, sequence)` covers the age cut, the sweep's oldest-event check and `resolve(At)` without reading the table.

`tenant_id` is written with every event and read by no query today, as in the PostgreSQL plugin, which keeps it as a column for tenant scoping: it is there so a tenant-filtered read or a per-tenant purge needs no change to the stored rows. The body carries it too, so a read never needs the column.

`evbk__fence` grows by one row per fence key ever used and nothing removes rows, as in the PostgreSQL plugin: fence keys are per outbox identity, so the table stays small in practice.

Invariants, each kept by one write transaction: `0 <= floor <= hwm` (a `CHECK`); the events of a partition are exactly the sequences `floor + 1 ..= hwm`; `bytes` is the sum of their `size`; `sequence_time` never falls as `sequence` rises.

`evbk__log`, `evbk__partition` and `evbk__fence` are `WITHOUT ROWID` tables: small rows, always looked up by their key. `evbk__event` keeps its rowid: bodies are often larger than the twentieth of a page that a `WITHOUT ROWID` row should stay under, and the key lives in the primary key's own index instead.

### 2.2 Column Types and Sizing

SQLite has type affinity, not types, and does not enforce a declared length; so every column is declared for what it holds, and every bounded column carries a `CHECK` that does enforce it.

| Column | Declared | Why |
|---|---|---|
| `topic_id` | `VARCHAR(1024)`, `CHECK (length(topic_id) <= 1024)` | a GTS instance id, at most `gts_id::GTS_ID_MAX_LENGTH` (1024) |
| `topic_uuid`, `log_id`, `fence_key`, `tenant_id` | `BLOB`, `CHECK (length(...) = 16)` where it is a key | 16 raw bytes, half the size of the text form; `topic_uuid` is `support::topic_uuid` (`GtsId::to_uuid`) |
| `partition`, `sequence`, `hwm`, `floor`, `bytes`, `size`, `last_seq`, `max_bytes` | `INTEGER` | 64-bit signed, SQLite's native integer |
| `sequence_time`, `head_time`, `provisioned_at` | `INTEGER` | microseconds since the Unix epoch, UTC: ordered, compact, and compared with no parsing |
| `max_age_ms` | `INTEGER` | milliseconds |
| `body` | `BLOB` | the unbounded event payload (§2.3) |

No column is `TEXT`: the only unbounded content is the body, and it is bytes.

### 2.3 Event Body

`body` is versioned JSON (`{"v": 1, ...}`) of every event field no column carries: `id`, `type_id`, `tenant_id`, `source`, `subject`, `subject_type`, `occurred_at`, `trace_parent`, `data`. `meta` (publisher-only) is not stored; `partition`, `sequence` and `sequence_time` come from the columns. It is the same encoding as the PostgreSQL plugin's. A body of an unknown version fails the read with `Internal`.

## 3. Component Model

### 3.1 Startup and Connections

`Provider::build` validates the settings (§5) and opens nothing. `start` spawns a startup task and returns at once; the task opens the file, migrates it (§3.3) and, where the role appends (`Ingest`, `Standalone`), starts the sweep (§4.8). A failure - the directory missing or unwritable, the file locked, a drifted schema - is logged by kind (never with the driver's text, which carries the path) and retried with backoff from 250 ms doubling to 30 s, until it succeeds or the backend is stopped. Until then every operation is `Unavailable`.

Every connection is opened with:

| Pragma | Value | Why |
|---|---|---|
| `journal_mode` | `WAL` | readers never wait for the writer, nor the writer for readers |
| `synchronous` | `FULL` (setting `synchronous`) | a commit is on disk when it returns (§4.2) |
| `auto_vacuum` | `INCREMENTAL` | the sweep can hand freed pages back (§4.8); takes effect only on a file created by this plugin |
| `busy_timeout` | 5000 ms (setting `busy_timeout_ms`) | waits for a lock held outside the hub (checkpoints, another process) |
| `foreign_keys` | `ON` | `evbk__partition` references `evbk__log` |

The pool holds `connections` connections (default 4). Writes serialize anyway (§3.2), so the pool is sized for concurrent reads; a waiting read (`Wait::For`) holds no connection while it waits.

`stop` cancels the startup task and the sweep, waits for them (until its deadline), and closes this instance's pool; every operation after it is `Unavailable`. Other instances over the file are untouched.

### 3.2 The Per-File Hub

Every instance over one file in this process shares a hub, found in a process-wide registry and kept alive while an instance holds it. The key is the file's canonical path once the file exists, so a symlink to it shares its hub; before the file is created, the canonical directory plus the file name, which is the path the file will canonicalize to. The lookup touches the file system, so startup runs it off the async runtime. The hub holds:

- **the heads** (`support::LocalHeads`): every append publishes the heads it committed; waiting reads and native watches of every instance over the file wake on them at once;
- **the write turn**: an async mutex every write transaction (`provision`, `append`, a sweep chunk, the incremental vacuum) takes before `BEGIN IMMEDIATE`, so writers queue in order instead of spinning in SQLite's busy handler;
- **the open turn**: held while an instance opens and migrates the file, so instances starting together do not race to create the schema.

### 3.3 Migrations

`sqlx::migrate!` over `src/migrations`, recorded in `_sqlx_migrations` in the same file. The schema has no `IF NOT EXISTS`: a file whose recorded migration no longer matches, or that records one this plugin does not know, keeps the backend `Unavailable` and logs why.

## 4. Operations

### 4.1 provision

One write transaction:

1. `INSERT INTO evbk__log ... ON CONFLICT (topic_uuid) DO NOTHING` with the claim's log id, the spec's partition count and retention.
2. Read the stored `log_id` and `partitions`. If `LogClaim::accepts(stored)` is false, roll back: `LogConflict`.
3. `UPDATE` the row to the claim's log id and the spec's retention - the sweep reads the bounds on its next pass, so a retention change takes effect within `sweep.interval_ms`.
4. Insert one `evbk__partition` row per partition of the **stored** count, `ON CONFLICT DO NOTHING`.

Answers `Ready { partitions }` with the stored count: a topic provisioned with 4 and asked again for 8 keeps 4. A count outside `1..=9999` is `InvalidConfig` (§6).

### 4.2 append

1. **Provisioned?** The topic's partition count, from the instance's cache or `evbk__log`; none is `NotProvisioned` - before the fence, so a fence that has passed its seq is no answer for a topic this backend does not hold.
2. Group the events by partition (`partition_for`) and encode their bodies, outside any lock.
3. Take the hub's write turn, then `BEGIN IMMEDIATE`: the file's write lock is taken up front, so no read-to-write upgrade can fail midway.
4. **Fence**: `INSERT INTO evbk__fence ... ON CONFLICT (fence_key) DO UPDATE SET last_seq = excluded.last_seq WHERE last_seq < excluded.last_seq`. No row changed means the fence has passed this seq: roll back, `Duplicate`.
5. **Per partition**, ascending: `UPDATE evbk__partition SET hwm = hwm + n, bytes = bytes + b, head_time = MAX(head_time, now) ... RETURNING hwm, head_time`, then one multi-row `INSERT` of the events at `hwm - n + 1 ..= hwm`, all stamped `head_time`.
6. `COMMIT`, release the write turn, publish each partition's new head to the hub. Answer `Heads`.

**A caller that goes away.** Up to `COMMIT` the caller's future owns the transaction: dropped before it - waiting for the write turn, in `BEGIN IMMEDIATE`, mid-insert - the transaction rolls back and nothing of the append is stored, its fence included (feature 0007 §2.2). From `COMMIT` on, the commit, the release of the write turn and the publish run in a task of their own, so a commit already in flight lands with its heads published; otherwise native watches would miss those heads until the partition's next append.

**Gapless.** One writer at a time, and the head is raised in the transaction that inserts every sequence below it: a sequence is either committed with its event or never assigned.

**Storage time.** `sequence_time` is the wall clock at the append, raised to the partition's `head_time` if the clock has stepped back: it never falls with the sequence, which `resolve(At)` and the age cut rely on. Every event of one append to one partition gets the same instant.

**Durability.** WAL with `synchronous=FULL` syncs the log on every commit: an acknowledged append survives a power loss. `synchronous: normal` syncs only at checkpoints: an acknowledged append survives a crash of the process, not of the machine (the last commits before a power loss may be lost, and the file stays consistent). An append either commits whole or not at all.

### 4.3 Deduplication Cases

| Case | Outcome |
|---|---|
| A retry of a committed append (same fence) | `Duplicate`, nothing stored |
| A late copy after a later fence of the key | `Duplicate`, nothing stored |
| A fence above the last with a gap | applied |
| An append that failed before `COMMIT`, retried | applied once: the rollback undid the fence row too |
| A fence key replayed on an unprovisioned topic | `NotProvisioned` (step 1 precedes the fence) |
| A restart on the same file | fences persist in `evbk__fence` |

### 4.4 read

`SELECT sequence, sequence_time, body FROM evbk__event WHERE topic_uuid = ? AND partition = ? AND sequence > ? ORDER BY sequence LIMIT ?` - a range scan of the primary key. No floor check: the sweep raises the floor and deletes below it in one transaction, so nothing at or below the floor is ever visible. A partition at or above the topic's count is `PartitionNotFound`.

Under `Wait::For(d)` the read is retried each time the hub publishes a head above the offset for the partition, and at least every second, until it returns events or `d` passes (`LocalHeads::read_waiting`).

### 4.5 resolve

From the partition row: `Latest` is `hwm` (never lowered by removal), `Earliest` is `floor`. `Exact(n)` within `[floor, hwm]` is `n`, outside it `OffsetOutOfRange`. `At(t)` is one below the first event with `sequence_time >= t` (rounded up to the microsecond), found on `evbk__event_time`; `Latest` when none is that late.

### 4.6 watch

`Watch::Native` over the hub's heads: first the current `hwm` of each named partition, read from the file, then every head an append in this process publishes above what the watch has yielded. An unprovisioned topic or a missing partition is yielded as an error item and ends the stream. Before startup has opened the file the stream yields one `Unavailable`.

### 4.7 query

One `TopicSegment` for a non-empty match: `start_sequence`/`end_sequence` and `start_time`/`end_time` of the first `limit` events in the requested sequence range, and one opaque segment entry, `{"from_sequence": floor + 1, "to_sequence": hwm}` - the partition's stored span, the only segment this plugin has. An empty match is an empty list.

### 4.8 Retention Sweep

Runs in every instance whose role appends, every `sweep.interval_ms` (default 1 s). Each pass lists, in one read outside the write lock, the partitions with a bound and `floor < hwm`, with their `bytes` and the `sequence_time` of their oldest event (one seek on `evbk__event_time` each). Only a partition past a bound - its oldest event older than `max_age`, or `bytes > max_bytes` - takes the write lock: an idle pass costs one read, however many partitions there are. For each, expired events are removed in chunks, one write transaction per chunk of at most `sweep.batch` events (default 10000), until a chunk comes back short:

1. Re-read `floor`, `hwm`, `bytes` in the transaction.
2. **Age**: the highest sequence with `sequence_time < now - max_age` (the last event stored before the cutoff; `sequence_time` never falls, so all below it are older).
3. **Bytes**: when `bytes > max_bytes`, walk the oldest events and take the first sequence at which their `size` adds up to the excess.
4. The cut is the higher of the two, capped at `floor + batch` and at `hwm`. `DELETE` the events in `(floor, cut]`, set `floor = cut` and subtract their size from `bytes`; commit.

After a pass that removed anything, `PRAGMA incremental_vacuum(2048)` hands up to 2048 freed pages back to the file system; the rest are reused by later appends or handed back on later passes.

**Granularity.** Per event: an event is removed on the first pass after it is past a bound, so it outlives its age by at most `sweep.interval_ms` plus the time the pass takes, and a partition is over its byte bound by at most what one interval appends. What stays is always a suffix of what was stored, and `Latest` is never lowered.

**What `max_bytes` counts.** The sum of the event bodies (`size`) in the partition, not disk usage: indexes, the other columns and free pages are not counted, and the file can be larger than the sum of the bounds.

Age is always measured from `sequence_time`, when the backend stored the event, never from the producer's `occurred_at`. Several instances over the file each run a sweep; they take turns at the write lock and each chunk re-reads the floor, so they never remove an event twice or one that is not past a bound.

## 5. Configuration

```yaml
backends:
  main:                                # the registry name; any name
    type: "gts.cf.core.events.backend.v1~cf.core.backend.sqlite.v1~"
    path: "${EB_DATA_DIR}/events.db"   # required; ${VAR} / ${VAR:-default} expanded; the directory must exist
    synchronous: full                  # or normal (§4.2). Default full
    busy_timeout_ms: 5000              # wait for a lock held outside the hub. Default 5000
    connections: 4                     # pool size. Default 4
    pool_acquire_timeout_ms: 5000      # how long an operation waits for a pooled connection. Default 5000
    sweep:                             # the retention sweep (§4.8); every field optional
      interval_ms: 1000                # between two passes. Default 1000
      batch: 10000                     # events removed per write transaction. Default 10000
```

- Settings are read by hand, not through serde, so an error names the setting and the rule it breaks and never quotes a value (feature 0007 §2.7). An unknown setting, at any level, is refused.
- `path` is a plain file path: empty, `:memory:` and `file:` URIs are refused - an in-memory database would break the durability `append` promises. The file is created if missing; its directory is not.
- Every count and interval is a positive integer. Intervals and timeouts are at most 86400000 ms (one day): past it, deadlines computed from them overflow, and SQLite takes its busy timeout as a 32-bit count of milliseconds. `sweep.batch` is at most 100000, the rows one sweep transaction loads under the write lock.
- The settings are the same in every process; the role decides only whether the sweep runs.

## 6. Limits

| Limit | Value | Why |
|---|---|---|
| Events per append | 1000 | 7 bound values each, well under SQLite's 32766 per statement |
| Partitions per topic | 1..=9999 | the PostgreSQL plugin's limit, so a topic fits on either |
| Processes per file | 1 | §1.1 |
| Write throughput | one writer at a time, per file | the price of gapless sequences and a single lock |
| Volume | small: everything is in one file and one table | no partitioning; retention deletes row by row |

## 7. Error Mapping

| Situation | `BackendError` |
|---|---|
| Startup not complete, file not openable, pool closed or timed out, I/O, SQLite `BUSY`, `LOCKED`, `NOMEM`, `READONLY`, `IOERR`, `FULL`, `CANTOPEN`, `PROTOCOL` (primary codes, any extended code) | `Unavailable` |
| No `evbk__log` row | `NotProvisioned` |
| Partition at or above the topic's count | `PartitionNotFound` |
| Foreign log id at provisioning | `LogConflict` |
| `Exact(n)` outside `[Earliest, Latest]` | `OffsetOutOfRange` |
| Unknown or invalid setting; partition count out of range | `InvalidConfig` |
| More than 1000 events in an append | `AppendFailed` |
| Any other SQLite code in `append` | `AppendFailed` |
| Any other SQLite code in `read`, `resolve`, `watch` or `query` | `ReadFailed` |
| Any other SQLite code in `provision` | `Internal` |
| A driver failure (decode, encode, missing row or column), an undecodable body | `Internal` |

A message names the operation and the SQLite result code, or the kind of driver failure, and nothing else: SQLite's message and the driver's text may quote stored values or the file path.

## 8. Operations Notes

- **Placement.** Put the file on a local disk. Network file systems break SQLite's locking and WAL's shared memory.
- **Files.** Beside `events.db` live `events.db-wal` and `events.db-shm` while it is open; copy all three, or none, and only while no process has it open - or use SQLite's online backup (`sqlite3 events.db ".backup copy.db"`), which is safe while the plugin runs.
- **Restoring from backup** brings back an older `evbk__fence`: fence keys that moved past it are accepted again, and their events stored twice; and an older `hwm`, so sequences already delivered are assigned again.
- **Disk full** makes appends `Unavailable` (`SQLITE_FULL`) until space is freed; tightening retention may not help, as the sweep's deletes need room in the WAL too.
- **Inspection.** The tables can be read with the `sqlite3` shell while the plugin runs (WAL readers do not block it): `SELECT topic_id, partition, floor, hwm, bytes FROM evbk__partition JOIN evbk__log USING (topic_uuid)` shows each partition's span and size.
- **A pre-existing file** created by something else keeps its own `auto_vacuum` mode; freed pages are then reused but not handed back.
- **Nothing detects a second process on the file.** The plugin takes no exclusive lock and checks for no other owner; running two processes over one file is an operator error that shows only as watches that miss heads (§1.1).
- **`evbk__fence` is never pruned** (§2.1); a file whose outbox identities churn accumulates one small row per identity.

## 9. Risks / Trade-offs

- **One writer per file.** Appends to any topic or partition serialize on the file's write lock; each holds it for one commit, including its sync under `synchronous=FULL`. Batching appends is what scales.
- **Row-by-row retention.** Deleting events costs in proportion to how many expire; a large backlog is worked through `sweep.batch` events per transaction, each holding the write lock briefly.
- **The WAL grows under a long reader.** A checkpoint cannot move past a snapshot a reader holds; a reader that stalls mid-transaction lets the WAL grow until it finishes.
- **The clock.** A wall clock stepped forward stamps events far ahead, and `head_time` keeps every later event of the partition at that instant until the clock catches up: their age is measured from it, and `resolve(At)` sees them as one instant.
- **Another process on the file** (unsupported): correct, but its heads do not wake this process's watches (§1.1), and its sweep runs too.
- **No metrics.** The plugin exports none; sweep failures are logged and retried next interval.

## 10. Later

- Monitoring hooks: file size, WAL size, partition bytes against bounds.
- A head poll in the hub, should a second process on the file ever be supported.
- `PRAGMA optimize` on close, should query plans drift on large files.
