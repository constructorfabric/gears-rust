# Technical Design - SQLite Event Broker Plugin

- [1. Overview](#1-overview)
  - [1.1 Role](#11-role)
  - [1.2 Contract Coverage](#12-contract-coverage)
  - [1.3 Today vs Target](#13-today-vs-target)
- [2. Domain Model](#2-domain-model)
  - [2.1 Plugin Tables](#21-plugin-tables)
  - [2.2 Planned Tables and Names](#22-planned-tables-and-names)
  - [2.3 Row Sizing](#23-row-sizing)
  - [2.4 Log Id Storage](#24-log-id-storage)
- [3. Component Model](#3-component-model)
  - [3.1 Crate Structure](#31-crate-structure)
  - [3.2 Provider and Build](#32-provider-and-build)
  - [3.3 Connection](#33-connection)
  - [3.4 Write Serialization](#34-write-serialization)
- [4. Operations](#4-operations)
  - [4.1 provision](#41-provision)
  - [4.2 append](#42-append)
  - [4.3 Deduplication Cases](#43-deduplication-cases)
  - [4.4 Chain-Sequence Check](#44-chain-sequence-check)
  - [4.5 read](#45-read)
  - [4.6 resolve](#46-resolve)
  - [4.7 watch](#47-watch)
  - [4.8 query](#48-query)
  - [4.9 Retention](#49-retention)
  - [4.10 list_partition_leaders](#410-list_partition_leaders)
- [5. Configuration](#5-configuration)
- [6. Error Mapping](#6-error-mapping)
- [7. Risks / Trade-offs](#7-risks--trade-offs)
- [8. Open Questions](#8-open-questions)
- [9. Work To Do](#9-work-to-do)

Tags: **[current]** is what the code at HEAD does; **[planned]** is required by event-broker features 0005-0007 or the backend decisions and is not in the code yet; **[removing]** is in the code and scheduled for removal. File references are to `src/` of this crate unless a path says otherwise.

## 1. Overview

### 1.1 Role

The storage backend for the event-broker gear that needs no external service: an event log in a SQLite file, or in memory, for a **single process**. It serves the backend type `gts.cf.core.events.backend.v1~cf.core.backend.sqlite.v1~` (`provider.rs:23`).

- **Owns its own database.** The gear's database keeps ingest and delivery metadata; the event log is not metadata, so the plugin opens the location its settings name and applies its own tables there (`lib.rs:17-21`, `connection.rs:1-11`).
- **Not a capability, not a gear.** It exposes a provider the gear links in; it owns no task and no timer. Retention is a trait method the gear drives on its own tick (`lib.rs:9-15`).
- **SDK only.** It depends on `event-broker-sdk`, never on the gear (`Cargo.toml`).
- **Single process.** One process opens a given file. Two processes on one file stay correct (§3.4) but are not a supported deployment; a cluster uses another backend (§7).

References:

- event-broker `docs/features/0005-backend-registry-and-routing.md` - registry entries, outbox groups and their identity (§2.5)
- event-broker `docs/features/0006-topic-provisioning.md` - provisioning attempts, log id compare-and-set (§2.3)
- event-broker `docs/features/0007-storage-backend-api.md` - the trait this plugin implements
- event-broker `docs/ADR/0001-offset-semantics.md` - sequences from 1, cursor 0
- `docs/arch/database/ADR/0001-cpt-cf-database-adr-object-namespacing.md` - table and index names

### 1.2 Contract Coverage

**[planned]** - what this plugin answers under the feature 0007 trait:

| Contract item | This plugin |
|---|---|
| `append` result | `Heads` |
| `dedup()` | `Fenced`: the fence row and the events commit in one transaction (§4.2) |
| `watch()` | `Poll(interval)`: the gear polls `resolve(Latest)` (§4.7) |
| `provision` | always `Ready` in one transaction; never `Pending` (§4.1) |
| `max_events_per_append()` | to be decided (§8) |
| Key to partition | to be decided; the recommendation is the Postgres plugin's `murmur3_x86_32(key, seed 0) & 0x7FFF_FFFF % partitions` (§8) |
| Sequences | per partition, SQLite `INTEGER` (64-bit) from 1, assigned under the database write lock |
| `Flush` | `Sync` and `Async` behave the same: every append commits before it returns |
| `read` with `Wait::For` | returns at once, as `Wait::No` (feature 0007 §2.3 allows it) |

### 1.3 Today vs Target

| Aspect | [current] | [planned] |
|---|---|---|
| Trait | `event_broker_sdk::EventBrokerBackend::{persist, resolve, read, query, list_partition_leaders, maintain}` (`backend.rs:84-571`, SDK `api.rs:577`) | `backend::Backend` (feature 0007 §2.1) |
| Provider | `EventBrokerBackendProvider::build_backend` (`provider.rs:33-58`) | `backend::Provider::build` (feature 0007 §2.7) |
| Partition | passed in by the caller (`persist(topic, partition, events)`, `backend.rs:86-92`) | mapped by the plugin from each event's key |
| Dedup | chain-sequence check per `(topic, partition)` (`backend.rs:118-135`) [removing] | `Fenced` per `(outbox_id, partition)` |
| Leader listing | `list_partition_leaders` (`backend.rs:400-435`) [removing] | none; not part of the trait |
| Provisioning, log id | none | `provision`, log table (§2.2, §4.1) |
| Watch | none | `Poll(interval)` |
| `SecurityContext` | taken by every method, unused (`_ctx`) | not part of the trait |

## 2. Domain Model

### 2.1 Plugin Tables

**[current]**, from `migrations/m20260819_000003_sqlite_backend.rs:28-72`:

```sql
CREATE TABLE IF NOT EXISTS event_broker_event (
    id             TEXT PRIMARY KEY NOT NULL,
    type_id        TEXT NOT NULL,
    topic          TEXT NOT NULL,
    tenant_id      TEXT NOT NULL,
    source         TEXT NOT NULL,
    subject        TEXT NOT NULL,
    subject_type   TEXT NOT NULL,
    occurred_at    TEXT NOT NULL,
    trace_parent   TEXT,
    data           TEXT NOT NULL,
    partition      INTEGER NOT NULL,
    sequence       INTEGER NOT NULL,
    sequence_time  TEXT NOT NULL,
    stored_bytes   INTEGER NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX IF NOT EXISTS event_broker_event_topic_partition_sequence_idx
    ON event_broker_event (topic, partition, sequence);

CREATE INDEX IF NOT EXISTS event_broker_event_tenant_idx
    ON event_broker_event (tenant_id);

-- Retention removes an aged prefix of one partition, so it scans
-- (topic, partition) ordered by sequence_time. Without this index that scan is
-- a full table read at exactly the moment the table is largest.
CREATE INDEX IF NOT EXISTS event_broker_event_retention_idx
    ON event_broker_event (topic, partition, sequence_time);

-- A timestamp SEEK resolves to the first event in (topic, partition) whose
-- `occurred_at` is at or after the requested instant. Without this index that
-- lookup is a partition scan, which is exactly what a bounded seek must not be.
CREATE INDEX IF NOT EXISTS event_broker_event_occurred_at_idx
    ON event_broker_event (topic, partition, occurred_at);

CREATE TABLE IF NOT EXISTS event_broker_partition_state (
    topic                 TEXT NOT NULL,
    partition             INTEGER NOT NULL,
    next_sequence         INTEGER NOT NULL,
    last_chain_sequence   INTEGER,
    event_count           INTEGER NOT NULL DEFAULT 0,
    stored_bytes          INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (topic, partition)
);
```

- **`event_broker_event`** is the log: one row per event, keyed `(topic, partition, sequence)` by the unique index. `topic` holds the topic's GTS id as text. The indexes serve the read and resolve lookups (unique), tenant scoping (`tenant_id`), retention (`sequence_time`) and `resolve(At)` (`occurred_at`).
- **`event_broker_partition_state`** is the bookkeeping per `(topic, partition)`: `next_sequence` is the next sequence to assign (the head is `next_sequence - 1`); `event_count` and `stored_bytes` are counted with every insert and removal, never derived from a sequence span (`entity/partition_state.rs:24-32`). `last_chain_sequence` is **[removing]** with the chain-sequence check (§4.4).
- **SecureORM.** Every query goes through `toolkit_db::secure`; no raw connection is held (`Cargo.toml`, `connection.rs:8-11`). The event entity is tenant-scoped (`#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]`, `entity/event.rs:13`); the partition state is `#[secure(unrestricted)]` (`entity/partition_state.rs:13`).
- **Tenants.** A topic carries events of many tenants, so the backend reads and writes with `AccessScope::allow_all()` throughout (`backend.rs`); `tenant_id` is stored and indexed for scoping, and tenant filtering happens above the backend.
- **Recreate, not migrate.** One idempotent `CREATE ... IF NOT EXISTS` migration, no versioned chain: a database file created before a column existed is recreated, not altered (`m20260819_000003_sqlite_backend.rs:5-9`).
- **Partition range.** `partition` is stored as `i32`; a `u32` partition above `i32::MAX` is refused (`backend.rs:96-97`).

### 2.2 Planned Tables and Names

**[planned]**. Names follow the database naming ADR under the namespace `event_broker_store` (shared with the MySQL plugin; a SQLite file and a MySQL database never coincide): tables `event_broker_store__<local>`, indexes `idx_<table>__<purpose>`, unique indexes `uq_<table>__<purpose>`, at most 63 bytes each. The current names are renamed:

| [current] | [planned] |
|---|---|
| `event_broker_event` | `event_broker_store__event` |
| `event_broker_event_topic_partition_sequence_idx` | `uq_event_broker_store__event__topic_partition_sequence` |
| `event_broker_event_tenant_idx` | `idx_event_broker_store__event__tenant` |
| `event_broker_event_retention_idx` | `idx_event_broker_store__event__retention` |
| `event_broker_event_occurred_at_idx` | `idx_event_broker_store__event__occurred_at` |
| `event_broker_partition_state` | `event_broker_store__partition`, without `last_chain_sequence` |
| none | `event_broker_store__fence` |
| none | `event_broker_store__log` |

```sql
CREATE TABLE IF NOT EXISTS event_broker_store__fence (     -- deduplication, one row per (outbox, ingest partition)
    outbox_id         INTEGER     NOT NULL,             -- the outbox group's 64-bit identity (feature 0005 §2.5)
    partition  INTEGER     NOT NULL,             -- OutboxMessage.partition_id
    last_seq          BIGINT      NOT NULL,             -- last applied OutboxMessage.seq
    PRIMARY KEY (outbox_id, partition)
);

CREATE TABLE IF NOT EXISTS event_broker_store__log (       -- one row per provisioned topic
    topic_uuid      VARCHAR(36)   PRIMARY KEY NOT NULL, -- GtsId::to_uuid(topic)
    topic_id        VARCHAR(1024) NOT NULL,
    log_id          VARCHAR(36)   NOT NULL,             -- feature 0006
    partitions      INTEGER       NOT NULL,
    max_age_seconds INTEGER,                            -- TopicSpec.retention; NULL = unbounded
    max_bytes       INTEGER,                            -- per partition; NULL = unbounded
    provisioned_at  VARCHAR(32)   NOT NULL              -- RFC 3339 UTC
);
```

- **Types.** The database belongs to the plugin, so the gear's portable-types rule does not bind it. Columns are declared with their sized format (`VARCHAR(36)` for a UUID, `VARCHAR(32)` for an RFC 3339 instant); SQLite stores them with `TEXT` affinity and does not enforce the length.
- **Fence rows** are few: one per ingest partition of each outbox group that writes here. Nothing is indexed per event for deduplication.
- **The rename is a recreate.** Under recreate-not-migrate (§2.1), a file holding the current tables is not converted; a deployment starts from a new file (§7).

### 2.3 Row Sizing

**[current]**. What a row counts against its partition's byte bound (`sizing.rs:31-48`): `FIXED_ROW_BYTES` (64, `sizing.rs:13`, an approximation of the uuid, tenant, integers and timestamps) plus the lengths of `type_id`, `source`, `subject`, `subject_type`, `trace_parent` and the JSON-serialized `data`.

- Stored on the row (`stored_bytes`), so a removal subtracts exactly what its insert added even if the rule changes.
- `topic` is not charged: it reaches the backend as an argument, not an event field (`sizing.rs:26-30`).
- An approximation, not a measurement: the only honest measurement, `PRAGMA page_count`, is not per row.

### 2.4 Log Id Storage

**[planned]**. Nothing stores a log id today. `event_broker_store__log` holds it, written by `provision` under the compare-and-set of feature 0006 §2.3 (§4.1).

## 3. Component Model

### 3.1 Crate Structure

```
src/
  lib.rs                 crate docs, module wiring, re-exports
  provider.rs            BACKEND_TYPE, SqliteBackendProvider (settings -> backend)
  options.rs             SqliteBackendOptions { path }, EventLogPath, DSN building
  connection.rs          open_event_log: connect, apply tables, MIGRATION_OWNER
  backend.rs             SqliteEventBackend: persist, resolve, read, query, list_partition_leaders, maintain
  sizing.rs              stored_bytes: a row's cost against the byte bound
  error.rs               ScopeError -> DbError inside transactions
  entity/                event.rs, partition_state.rs (SeaORM entities)
  migrations/            m20260819_000003_sqlite_backend.rs (the DDL of §2.1)
  *_tests.rs             backend, provider, options, sizing, footprint
  test_support.rs        test helpers
```

### 3.2 Provider and Build

- **[current]** `build_backend` deserializes the settings into `SqliteBackendOptions`, then opens the database and applies the tables before returning (`provider.rs:46-57`). An open or migration failure fails the build with `Unavailable`.
- **[current]** A settings error is `InvalidConfig` carrying the deserializer's text after the backend type (`provider.rs:51-55`); an `Unavailable` from opening carries the path in `detail` (`connection.rs:65-69`).
- **[planned]** `build` validates the settings and connects lazily, on the first operation: a file that cannot be opened fails operations with `Unavailable`, never the start (feature 0007 §2.7).
- **[planned]** A build error names the setting and the rule broken, never a value or a deserializer message quoting one.

### 3.3 Connection

**[current]** (`connection.rs:30-63`):

- `toolkit_db::connect_db(dsn, opts)` returns the secure `Db`; the tables go through `toolkit_db`'s migration runner under the owner `event-broker-sqlite-backend` (`connection.rs:21`), separate from the gear's migration history.
- `:memory:` gets a pool of exactly one connection (`max_conns = min_conns = 1`, `connection.rs:33-44`): SQLite gives each connection to `:memory:` its own database.
- A file opens with `sqlite://<path>?mode=rwc`, which creates it on first use (`options.rs:61-70`); `toolkit_db` creates missing parent directories (`create_sqlite_dirs`, default on, `libs/toolkit-db/src/lib.rs:344, 439`).
- `toolkit_db` defaults for a file: WAL journal, `synchronous = NORMAL`, `busy_timeout` 5000 ms; for `:memory:`: `DELETE` journal, no busy timeout (`libs/toolkit-db/src/lib.rs:360, 461-503`). The DSN passes no pragmas of its own.
- One pool per build: two registry entries naming one file get one pool each (`provider.rs:39-41`).

### 3.4 Write Serialization

SQLite has one writer per database. The append relies on it for sequence assignment and for deduplication.

- **[current]** Transactions begin with a deferred `BEGIN` (`DBProvider::transaction`, `libs/toolkit-db/src/secure/db.rs:513-542`), and `persist` reads the partition state before it writes (`backend.rs:104-116`). Under WAL, a second writer whose read snapshot went stale gets `SQLITE_BUSY_SNAPSHOT` at its first write and fails with `PersistFailed`; the unique index keeps sequences distinct either way.
- **[planned]** The append takes the write lock before it reads anything: `BEGIN IMMEDIATE`, waiting up to the busy timeout. Fence, provisioned check and partition state are then read under the lock, so two appends, from one process or two on one file, serialize and never fail on a stale snapshot. Feature 0007 AC-7 depends on it.
- **[planned/open]** `toolkit_db` offers no immediate begin today (`TxConfig` carries isolation and access mode only, `libs/toolkit-db/src/secure/tx_config.rs:115-120`). Either it gains one, or the fence upsert as the first statement of a deferred transaction takes the write lock before any read, which gives the same order.
- A wait longer than the busy timeout is `SQLITE_BUSY`, mapped to `Unavailable`; the outbox retries.

## 4. Operations

### 4.1 provision

**[planned]**. `provision(TopicSpec { topic, partitions }, LogClaim { replacing, new })`:

```sql
BEGIN IMMEDIATE;
INSERT INTO event_broker_store__log (topic_uuid, topic_id, log_id, partitions, provisioned_at)
VALUES ($t, $topic_id, $new, $partitions, $now)
ON CONFLICT (topic_uuid) DO NOTHING;
SELECT log_id, partitions FROM event_broker_store__log WHERE topic_uuid = $t;
--   log_id = $new                      -> this attempt (fresh, or continued)
--   log_id = replacing (Log(id))       -> UPDATE event_broker_store__log SET log_id = $new WHERE topic_uuid = $t
--   anything else                      -> ROLLBACK; LogConflict
-- per partition p in 0..stored partitions:
INSERT INTO event_broker_store__partition (topic, partition, next_sequence) VALUES ($topic, p, 1)
ON CONFLICT (topic, partition) DO NOTHING;
COMMIT;
-> Ready { partitions: stored partitions }
```

- Tables already exist (§3.3), so provisioning writes rows only and is one transaction: a failure leaves nothing.
- The stored `partitions` is the count reported; a different count requested later is the gear's concern (feature 0006 §3.1).
- `partitions` above `i32::MAX` is refused with `InvalidConfig` (§2.1).

### 4.2 append

**[current]** `persist(ctx, topic, partition, events)` (`backend.rs:86-228`), one transaction:

1. read `event_broker_partition_state` for `(topic, partition)`; absent means `next_sequence = 1` (`backend.rs:104-116`);
2. the chain-sequence check, returning `Ok(())` with nothing written on a recognised retry (`backend.rs:118-135`) [removing];
3. one `secure_insert` per event, sequences `next_sequence ..`, `sequence_time = now` (`backend.rs:144-176`);
4. update the state row (`next_sequence`, `last_chain_sequence`, `event_count`, `stored_bytes`), inserting it when absent (`backend.rs:183-222`).

It returns `()`: no head. An empty slice returns at once (`backend.rs:93-95`).

**[planned]** `append(&AppendRequest { fence: { outbox, partition, seq }, topic, events, flush })`:

```sql
BEGIN IMMEDIATE;                                              -- the write lock, first (§3.4)
-- (1) fence: first statement
INSERT INTO event_broker_store__fence (outbox_id, partition, last_seq) VALUES ($o, $p, $seq)
ON CONFLICT (outbox_id, partition)
DO UPDATE SET last_seq = excluded.last_seq
WHERE last_seq < excluded.last_seq;
--   0 rows changed -> ROLLBACK; Ok(Duplicate)

-- (2) provisioned?
SELECT partitions FROM event_broker_store__log WHERE topic_uuid = $t;
--   no row -> ROLLBACK; NotProvisioned

-- (3) per event: p = partition_of(key, partitions); per partition touched, ascending:
UPDATE event_broker_store__partition
   SET next_sequence = next_sequence + $n,
       event_count   = event_count + $n,
       stored_bytes  = stored_bytes + $bytes
 WHERE topic = $topic AND partition = $p
RETURNING next_sequence;                                      -- the partition's events take [returned - n, returned - 1], in request order

-- (4) the events
INSERT INTO event_broker_store__event (...) VALUES (...);     -- one row per event
COMMIT;
-> Ok(Heads([(topic, p) -> returned - 1, ...]))
```

- The fence row and the events commit together or not at all; that is `Fenced`.
- A rollback at (1) or (2) undoes the fence write, so a `NotProvisioned` append is retried later with the same fence and is not taken for a duplicate.
- Events of one key keep request order: they map to one partition and take its sequences in order.
- `events.len()` never exceeds `max_events_per_append()` (§8).

### 4.3 Deduplication Cases

**[planned]**:

| Case | What happens | Result |
|---|---|---|
| Retry after an ambiguous commit (reply lost after `COMMIT`) | step (1): `last_seq < seq` is false, 0 rows | `Duplicate`, nothing written |
| Dropped call still open, retry arrives | the dropped call's transaction holds the write lock until it commits or rolls back; the retry's `BEGIN IMMEDIATE` waits for it. If it committed, the retry's step (1) finds the fence and gets 0 rows; if it rolled back, the retry stores the message once | a late commit can never land after its retry or a later message (AC-10) |
| Retry waits past the busy timeout | `SQLITE_BUSY` -> `Unavailable`; the outbox retries | stored once |
| Hard restart of ingest | the outbox re-delivers unacked messages; each meets step (1) | applied ones `Duplicate`, the rest stored |
| Process crash (gear and plugin share it) | an open transaction is rolled back by the journal on the next open; fence, events and head always agree | consistent |
| `:memory:` | fence rows live and die with the log, as feature 0007 §2.2 requires; a restart starts both empty | consistent; the events are gone by design |
| Two outbox groups, or two deployments, writing one file | different `outbox_id`, different rows | independent |

### 4.4 Chain-Sequence Check

**[removing]** `backend.rs:118-135` and `last_chain_sequence` (`entity/partition_state.rs:20-23`).

- Today a `persist` whose every chained or monotonic event has a producer chain sequence at or below the partition's `last_chain_sequence` is taken for a retry and stores nothing.
- It goes because it keys on the producer's chain, which ingest already checks before enqueue (ADR-0004), not on the outbox message: it cannot tell a retry from a different producer's events in the same partition, never dedups stateless events, and cannot exist once the plugin, not the caller, picks the partition.
- The fence (§4.2 step (1)) replaces it: one `last_seq` per `(outbox_id, partition)`, independent of producers, modes and backend partitions.
- The gear's `redelivery_after_persist_is_a_safe_noop` (`event-broker/src/infra/workers/ingest_outbox.rs:295`) then asserts `Duplicate`.

### 4.5 read

**[current]** (`backend.rs:317-341`):

```sql
SELECT * FROM event_broker_event
 WHERE topic = $topic AND partition = $p AND sequence > $after
 ORDER BY sequence LIMIT $max_count;
```

Served by the unique index. Events come back with `partition`, `sequence` and `sequence_time` set and `meta` empty (`backend.rs:573-589`).

**[planned]** The same query under `read(at, offset, max_events, wait)`; `Wait::For` returns at once; no `event_broker_store__log` row is `NotProvisioned`.

### 4.6 resolve

**[current]** (`backend.rs:230-315`):

- `Latest` is the ceiling, `next_sequence - 1` from the state row, or 0 when there is none: the highest sequence ever assigned, so removing events never lowers it.
- `Earliest` is the floor, the oldest stored sequence minus 1, or the ceiling when nothing is stored.
- `At(t)` is the first event with `occurred_at >= t`, ordered by `occurred_at`, minus 1, through the `occurred_at` index; nothing that recent resolves to the ceiling.
- `Exact(n)` is `n` when `floor <= n <= ceiling`, else `OffsetOutOfRange` with `BelowFloor` or `AboveCeiling`.

**[planned]** No `event_broker_store__log` row is `NotProvisioned`; `At(t)` orders by `occurred_at, sequence` so equal timestamps resolve to the lowest sequence.

### 4.7 watch

**[planned]** `watch()` returns `Watch::Poll(interval)`: the gear polls `resolve(Latest)`, one primary-key read of `event_broker_store__partition` per watched partition. The interval is open (§8).

### 4.8 query

**[current]** (`backend.rs:343-398`): the rows of `(topic, partition)` within `range` (start and end inclusive, at most `range.limit`), described as one synthetic segment with its first and last sequence and time and its event count. The plugin has no segmented storage; segment shape is backend-specific (feature 0003).

### 4.9 Retention

**[current]** `maintain` (`backend.rs:437-570`), driven by the gear's retention worker, one transaction per `(topic, partition)`:

- No state row: an empty report.
- Walks the oldest rows in sequence order, at most 4096 per pass (`MAX_ROWS_PER_RETENTION_PASS`, `backend.rs:40`), removing each while it is older than `oldest_permitted` by `sequence_time`, or while the partition is above `max_stored_bytes`; the first row within both bounds ends the walk, so removal is always a prefix.
- Deletes the prefix, subtracts the counts and bytes from the state row, and reports removed and remaining events and bytes and the oldest surviving sequence. `removed_events` is the count the `DELETE` reports.
- A partition far past its bounds converges over several passes.

**[planned]** `maintain` leaves the trait and the gear's retention worker is removed (feature 0007 §2.6). The plugin runs the same pass from its own worker, started by `build` and stopped by its `stop` token, with the bounds `provision` stored in `event_broker_store__log` (`max_age_seconds`, `max_bytes`); single process, so no coordination is needed.

**[planned]** Never touches `event_broker_store__fence` or `event_broker_store__log` rows: fence state lives as long as the topics it guards (feature 0007 §2.2).

### 4.10 list_partition_leaders

**[current, removing]** (`backend.rs:400-435`): every partition with a state row, each with an empty `endpoint`; a partition emptied by retention is still listed. Not part of the feature 0007 trait.

## 5. Configuration

**[current]** (`options.rs:16-59`):

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SqliteBackendOptions {
    /// A filesystem path, or `:memory:` for a log that lives only as long as the process.
    pub path: EventLogPath,
}

#[serde(from = "String")]
pub enum EventLogPath {
    #[default]
    InMemory,          // ":memory:"
    File(PathBuf),     // created, with its parent directories, if missing
}
```

```yaml
backends:
  sqlite-main:
    type: "gts.cf.core.events.backend.v1~cf.core.backend.sqlite.v1~"
    path: "~/eb/events.db"   # or ":memory:", the default
```

- `path` is the only setting; an unknown key is refused, since a misspelled `path` would otherwise leave the log in memory (`options.rs:16-20`).
- The default is `:memory:`, deliberately not a file: the plugin is told no data directory, so any file default would be a location the operator did not choose (`options.rs:33-41`).
- A leading `~/` expands to the user's home; `~` alone and `~user` are left as written (`options.rs:90-104`). A relative path resolves against the working directory (`options.rs:106-121`).
- **[open]** A setting for the watch poll interval (§8).

## 6. Error Mapping

| Situation | [current] | [planned] `BackendError` |
|---|---|---|
| Unknown or invalid setting | `InvalidConfig`, deserializer text in `detail` (`provider.rs:51-55`) | `InvalidConfig`, naming the setting and rule |
| Database cannot be opened, tables cannot be applied | `Unavailable` at build (`connection.rs:46-60`) | `Unavailable` on the operation (lazy connect) |
| `SQLITE_BUSY`, `SQLITE_BUSY_SNAPSHOT` | `PersistFailed`, `ReadFailed` or `RetentionFailed` | `Unavailable` |
| Any other database failure | `PersistFailed`, `ReadFailed` or `RetentionFailed` by operation (`backend.rs:42-61`) | unchanged |
| Partition above `i32::MAX` | `PersistFailed` or `ReadFailed` (`backend.rs:96-97, 237-238`) | cannot occur: partitions come from the log row, bounded at `provision` |
| `Exact(n)` outside `[Earliest, Latest]` | `OffsetOutOfRange` (`backend.rs:298-312`) | unchanged |
| No `event_broker_store__log` row | none | `NotProvisioned` |
| Foreign log id at provisioning | none | `LogConflict` |

Messages name the operation and the rule broken, never a setting value, stored value or event content.

## 7. Risks / Trade-offs

- **Single process.** The intended use is one process per file: a development setup, a test, a single-node deployment. Two processes on one local file stay correct through the write lock (§3.4) but contend for it; a network filesystem is outside SQLite's locking guarantees. A cluster uses the Postgres, MySQL or Kafka plugin.
- **`:memory:` lifetime.** The log, its fence rows and its log id live as long as the process. After a restart the gear's binding still says `provisioned` while the backend holds nothing: `append` answers `NotProvisioned` until the topic is provisioned again (feature 0006, ADR-0008).
- **Recreate, not migrate.** A schema change, the planned rename included (§2.2), means a new file; events in the old one are not carried over.
- **Durability under a host crash.** WAL with `synchronous = NORMAL` (§3.3) survives a process crash, as feature 0007 §2.2 requires, but a power loss or OS crash may roll back the last commits. Fence and events roll back together, yet the ingest outbox, in another database, may already have acked them: those events are lost. `synchronous=FULL` in the DSN closes the gap at a write-latency cost.
- **Approximate byte accounting.** The byte bound counts `stored_bytes` (§2.3), not pages on disk; the file is not shrunk by removals without `VACUUM`.
- **Write-lock contention.** Every append, provision and retention pass takes the one write lock; appends are bounded by one commit at a time across all topics in the file.

## 8. Open Questions

- **Key to partition.** The function `partition_of(key, partitions)`. Recommendation: the Postgres plugin's `murmur3_x86_32(key, seed 0) & 0x7FFF_FFFF % partitions`, so a topic moved between the two plugins keeps per-key placement.
- **`max_events_per_append()`.** The value. Each event is one `INSERT` today (`backend.rs:172-176`), so SQLite's bound-parameter limit does not constrain it; the write-lock hold time does.
- **Poll interval.** The `Watch::Poll` interval, and whether it is a setting beside `path`.
- **Topic key.** Whether `event_broker_store__event` and `event_broker_store__partition` keep `topic` (the GTS id as text) or switch to `topic_uuid`, as `event_broker_store__log` uses.

## 9. Work To Do

- Implement `backend::Backend` and `backend::Provider` (feature 0007 §2.1, §2.7) in place of `EventBrokerBackend` and `EventBrokerBackendProvider`.
- `append` returning `Heads`; `watch` returning `Poll`; `dedup()` returning `Fenced` with `event_broker_store__fence` in the append transaction, taken under the write lock first (§3.4, §4.2).
- Key-to-partition mapping and `max_events_per_append()` (§8).
- Remove the chain-sequence check (`backend.rs:118-135`), `last_chain_sequence`, and `list_partition_leaders`.
- `event_broker_store__log` and `provision` with the log id compare-and-set (§4.1); `NotProvisioned` from `append`, `read` and `resolve`.
- Rename every table and index to the `event_broker_store` namespace (§2.2).
- Lazy build and secret-safe errors (§3.2, §6).
- Unit tests per feature 0007 §10 and feature 0006 §9: dedup (plain retry, late copy, `seq` gaps, two `outbox_id`s, state across a restart), concurrent appends to one partition, provisioning compare-and-set.
