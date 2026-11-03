# Technical Design - Postgres Event Broker Plugin

- [1. Overview](#1-overview)
  - [1.1 Role](#11-role)
  - [1.2 Contract Coverage](#12-contract-coverage)
- [2. Domain Model](#2-domain-model)
  - [2.1 Plugin Tables](#21-plugin-tables)
  - [2.2 Event Tables and Segments](#22-event-tables-and-segments)
  - [2.3 NOTIFY Payload](#23-notify-payload)
- [3. Component Model](#3-component-model)
  - [3.1 Connections](#31-connections)
  - [3.2 Migrations](#32-migrations)
  - [3.3 Lock Order](#33-lock-order)
- [4. Operations](#4-operations)
  - [4.1 provision](#41-provision)
  - [4.2 append](#42-append)
  - [4.3 Deduplication Cases](#43-deduplication-cases)
  - [4.4 Concurrent Ingest Partitions and Visibility](#44-concurrent-ingest-partitions-and-visibility)
  - [4.5 read](#45-read)
  - [4.6 resolve](#46-resolve)
  - [4.7 watch](#47-watch)
  - [4.8 query](#48-query)
  - [4.9 Maintenance](#49-maintenance)
  - [4.10 Route Changes, Foreign Logs, Lost Data](#410-route-changes-foreign-logs-lost-data)
- [5. Configuration](#5-configuration)
- [6. Error Mapping](#6-error-mapping)
- [7. Risks / Trade-offs](#7-risks--trade-offs)
- [8. Open Questions](#8-open-questions)
- [9. Later](#9-later)

## 1. Overview

### 1.1 Role

A storage backend for the event-broker gear over PostgreSQL. It implements `backend::Backend` and ships a `backend::Provider` for the backend type `gts.cf.core.events.backend.v1~cf.core.backend.postgres.v1~` (event-broker feature 0007). One `backends` registry entry (feature 0005 §2.1) is one database this plugin owns: the gear's own database is never used.

References:

- event-broker `docs/features/0005-backend-registry-and-routing.md` - registry entries, outbox groups and their identity
- event-broker `docs/features/0006-topic-provisioning.md` - provisioning attempts, log id
- event-broker `docs/features/0007-storage-backend-api.md` - the trait this plugin implements
- event-broker `docs/ADR/0001-offset-semantics.md` - sequences from 1, cursor 0
- event-broker `docs/ADR/0008-topic-log-identity.md` - log id checks (open)

### 1.2 Contract Coverage

| Contract item | This plugin |
|---|---|
| `append` result | `Heads` |
| `dedup()` | `Fenced`: the fence row and the events commit in one transaction |
| `watch()` | `Native`: `LISTEN/NOTIFY` |
| `provision` | always `Ready` in one transaction; never `Pending` |
| `max_events_per_append()` | 1000 |
| Key to partition | `(murmur3_x86_32(key, 0) & 0x7FFF_FFFF) % partitions` (`event_broker_sdk::backend::support::partition_for`), fixed for the life of the plugin |
| Sequences | per partition, `BIGINT` from 1, assigned under a row lock; visible in sequence order |
| `Flush` | `Sync` and `Async` behave the same: every append commits before it returns |

## 2. Domain Model

### 2.1 Plugin Tables

```sql
CREATE TABLE evbk__log (                              -- one row per provisioned topic
    topic_uuid     UUID          PRIMARY KEY,      -- GtsId::to_uuid(topic)
    topic_id       VARCHAR(1024) NOT NULL,
    log_id         UUID          NOT NULL,         -- feature 0006
    partitions     INTEGER       NOT NULL,         -- at most 9999 (table naming, §2.2)
    max_age        INTERVAL,                       -- TopicSpec.retention; NULL = unbounded
    max_bytes      BIGINT,                         -- per partition; NULL = unbounded
    provisioned_at TIMESTAMPTZ   NOT NULL
);

CREATE TABLE evbk__partition (                        -- one row per (topic, partition); append writes hwm
    topic_uuid     UUID    NOT NULL REFERENCES evbk__log,
    partition      INTEGER     NOT NULL,
    table_name     VARCHAR(63) NOT NULL UNIQUE,    -- the partitioned table, as created (§2.2)
    hwm            BIGINT      NOT NULL DEFAULT 0, -- highest ever assigned; 0 = nothing yet
    PRIMARY KEY (topic_uuid, partition)
);

CREATE TABLE evbk__retention (                        -- one row per (topic, partition); written by retention only
    topic_uuid     UUID    NOT NULL,
    partition      INTEGER NOT NULL,
    floor          BIGINT  NOT NULL DEFAULT 0,     -- highest removed sequence; stored = (floor, hwm]
    PRIMARY KEY (topic_uuid, partition),
    FOREIGN KEY (topic_uuid, partition) REFERENCES evbk__partition
);

CREATE TABLE evbk__fence (                            -- deduplication, one row per fence key
    fence_key         UUID     PRIMARY KEY,        -- FenceKey's 16 bytes, opaque to the plugin
    last_seq          BIGINT   NOT NULL            -- last applied FenceSeq
);

CREATE TYPE evbk__segment_state AS ENUM ('open', 'sealed', 'expired', 'detaching');

CREATE TABLE evbk__segment (                          -- one row per segment table (§4.9)
    topic_uuid     UUID                NOT NULL,
    partition      INTEGER             NOT NULL,
    first_seq      BIGINT              NOT NULL,   -- [first_seq, end_seq): the table's bound
    end_seq        BIGINT              NOT NULL,
    table_name     VARCHAR(63)         NOT NULL UNIQUE,   -- the segment table, as created (§2.2)
    state          evbk__segment_state NOT NULL,
    bytes          BIGINT              NOT NULL DEFAULT 0,  -- pg_total_relation_size of the table when sealed; 0 while open
    newest_at      TIMESTAMPTZ,                    -- the last event's sequence_time when sealed; NULL while open or empty
    changed_at     TIMESTAMPTZ         NOT NULL,   -- last change of any column
    PRIMARY KEY (topic_uuid, partition, first_seq),
    FOREIGN KEY (topic_uuid, partition) REFERENCES evbk__partition
);
```

| Table | Written by |
|---|---|
| `evbk__log` | `provision` |
| `evbk__partition` | `provision` inserts (with `table_name`); `append` raises `hwm` |
| `evbk__retention` | `provision` inserts; retention raises `floor` |
| `evbk__fence` | `append` |
| `evbk__segment` | `provision` and the extender insert `open` rows; the sealer writes `bytes` and `newest_at` and seals; retention expires; the detacher marks `detaching` and deletes; reconcile deletes or adopts a row |

Each row has one kind of writer. `append` holds the `evbk__partition` row lock until commit (§4.4); were retention to write the same row, an append would wait out every expiry `DELETE` and segment drop, and fail on `lock_timeout`. Stored figures are derived:

```
stored_events = hwm - floor            -- one snapshot of both rows; sequences are gap-free and retention removes a prefix
stored_size   = SUM(bytes) of the sealed segments + pg_total_relation_size of each open one   -- on disk (§2.2)
```

Native PostgreSQL types: this database belongs to the plugin, so the gear's portable-types rule does not apply. `evbk__fence` holds one row per fence key the caller uses - a handful; nothing is indexed per event for deduplication.

- **`evbk__fence` is never cleaned up.** It is bounded by the fence keys a caller ever uses, not by traffic: an append updates its key's row in place. The ingest outbox uses one key per outbox partition (feature 0007 §4.1), so a deployment holds a fixed few; a key the caller stops using leaves one dead row.
- The plugin cannot tell a dead key from an idle one: keys are opaque. Deleting a live key's row would admit a redelivered message again, breaking exactly-once, and fence state lives as long as the data it guards (feature 0007 §2.2), which here is the whole database, since one key spans every topic. Retiring a key is the caller's decision, through an operation the backend API does not have yet.

### 2.2 Event Tables and Segments

Each `(topic, partition)` is one partitioned table; each of its PostgreSQL partitions is a **segment**, a contiguous sequence range.

```
evbk__{topic_uuid}_p{partition}                       partitioned table, PARTITION BY RANGE (sequence)
evbk__{topic_uuid}_p{partition}_s{first_sequence}     segment, FOR VALUES FROM (first) TO (next first)

evbk            the plugin's db_namespace (docs/arch/database/ADR/0001: <db_namespace>__<local_name>)
topic_uuid      GtsId::to_uuid(topic) as 32 lowercase hex digits, no dashes
partition       decimal, at most 4 digits (partitions <= 9999)
first_sequence  the segment's first sequence, 16 lowercase hex digits, zero-padded (covers all of i64)
length          6 + 32 + 2 + 4 + 2 + 16 = 62 <= 63, PostgreSQL's identifier limit
```

Every other object follows the same ADR: indexes `idx_<table>__<purpose>`, e.g. `idx_evbk__{topic_uuid}_p{partition}__time` below. The prefix stays even in a dedicated database: an entry's `url` may point at a database other objects share.

```sql
CREATE TABLE evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3 (
    sequence       BIGINT        NOT NULL,         -- from 1 (ADR-0001)
    sequence_time  TIMESTAMPTZ   NOT NULL,         -- when it was appended; retention age and resolve(At)
    tenant_id      UUID          NOT NULL,         -- tenant scoping
    body           BYTEA         NOT NULL,         -- the event, encoded as below; opaque to PostgreSQL
    PRIMARY KEY (sequence)
) PARTITION BY RANGE (sequence);
CREATE INDEX idx_evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3__time
    ON evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3 USING brin (sequence_time);   -- resolve(At)

CREATE TABLE evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3_s0000000000000001
    PARTITION OF evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3 FOR VALUES FROM (1) TO (1000001);
```

- Names are derived from `(topic_uuid, partition, first_sequence)` when a table is created, then stored with its catalog row: `evbk__partition.table_name` for the partitioned table, `evbk__segment.table_name` for a segment. Every later use reads the stored name, so a change to these rules leaves existing tables as they are, and a segment's name extends its parent's stored name.
- Only the fields positions, retention or tenant scoping use are columns; everything else of the event lives in `body`. `type_id` and `subject_type` are not columns: no read filters by them (§4.5), and a filtered read brings the columns it needs.
- `occurred_at` is not a column: it is the producer's claim, untrusted and unordered within a partition, so it never decides a position or an age. It travels in `body` as event data.
- `body` is the plugin's own versioned encoding, JSON bytes: `{"v":1, "id", "type_id", "tenant_id", "source", "subject", "subject_type", "occurred_at", "trace_parent", "data"}`. `meta` is not stored (publisher-only, stripped on read); `partition`, `sequence` and `sequence_time` are columns. A reader refuses a `v` it does not know with `Internal`.
- Sizes are what the tables take on disk, all overhead included: `pg_total_relation_size` of each segment table, heap, TOAST, the primary key and the `brin` index. No event carries a size and no counter adds them up. A segment is measured once, when it is sealed full (§4.9): its table is insert-only, so the figure is final, but for the visibility map and free space map a vacuum may add later. `max_bytes` bounds this size per partition: the sealed segments as measured, and every open segment as it is now, the empty ones made ahead of the head included (a few tens of KB each). The contract leaves the unit to the backend (feature 0007 §2).
- `sequence_time` rises with `sequence` (§4.2), the order a block-range index summarizes best: the `brin` index costs a few bytes per range of blocks and lets `resolve(At)` skip whole ranges (§4.6). PostgreSQL gives every segment its own copy of the parent's indexes.
- A segment's range is fixed when it is created (§4.9); sizes may differ from segment to segment, and the suffix keeps every name exact.

### 2.3 NOTIFY Payload

```
channel  evbk__head
payload  "{topic_uuid hex}:{partition}:{sequence}"     e.g. "3f2a...b7a3:3:42"
```

One statement per append carries every head the append left, one notification each:

```sql
SELECT pg_notify('evbk__head', payload) FROM UNNEST($payloads::text[]) AS payload;
```

A transaction that notifies holds a lock shared by the whole cluster from its commit until the commit is flushed, so notifying commits run one at a time. `head_notify` (§5) decides who pays for that:

- `in_transaction` (default): the append sends it before its `COMMIT`, and PostgreSQL delivers it on commit, in commit order. Every append waits its turn at commit.
- `after_commit`: after the commit, the append leaves its heads, latest per partition, to the instance's notifier worker, which sends what has gathered in one transaction with `synchronous_commit = off` (a lost notification is lost before the flush or after it alike). Appends commit in parallel again; notifications arrive one hop later and in no particular order across instances, which a listener tolerates since a head at or below a known one is dropped. A failed send keeps its heads for the next pass. A crash between a commit and its send loses that notification (§7).

## 3. Component Model

### 3.1 Connections

The plugin talks to PostgreSQL through `sqlx` directly, following the cluster gear's Postgres plugin (`gears/system/cluster/plugins/postgres-cluster-plugin`): dynamic table names, transactional DDL, `LISTEN/NOTIFY` and pool connect hooks have no SeaORM equivalent. The crate allows the direct use and says why in its `lib.rs` documentation:

```rust
// Why `sqlx` directly, not `libs/toolkit-db`: dynamic per-topic tables, transactional DDL,
// LISTEN/NOTIFY on a session-pinned connection, and pool connect hooks.
#![allow(unknown_lints)]
#![allow(de0706_no_direct_sqlx)]
```

- **Role.** The gear builds the backend with the process's role (feature 0007 §2.7): `ingest`, `delivery` or `standalone` (both). Only an `ingest` or `standalone` process runs the maintenance (§4.9) and holds the background pool; a role the plugin does not know yet runs none.
- **Main pool.** `PgPoolOptions` with `max_connections = pools.main` and `acquire_timeout`; `after_connect` sets `synchronous_commit = on`, `search_path = {schema}`, `lock_timeout = 5s` and `idle_in_transaction_session_timeout = 30s` - the last two bound a retry waiting on a hung transaction's fence row (§4.3). Constants, not settings. Used by every operation: `append`, `read`, `resolve`, `query`, `provision`.
- **Background pool.** The same setup, `max_connections = pools.background`, at least one connection per pooled maintenance worker (`extender + sealer + retention + reconcile + 1` for the rescan), so a worker never waits for one. Connects on first use; used by every maintenance worker but the detacher.
- **Detacher connections.** Each detacher worker opens one connection of its own outside the pool, with no `lock_timeout` or `statement_timeout`, because `DETACH ... CONCURRENTLY` refuses a transaction block and waits out the transactions that use the parent. A connection is dropped after any error and reopened on the next pass.
- **`LISTEN` connection.** A `PgListener` on its own connection outside the pools, held in `LISTEN evbk__head`. An entry holds at most `pools.main + 1` connections in a delivery process, and `pools.main + 1 + pools.background + detacher.concurrency` where the maintenance runs.
- **Notifier.** Under `head_notify: after_commit` (§2.3), one worker borrows a main-pool connection for each send.
- **Names.** Each kind of connection sets its `application_name`, so `pg_stat_activity` tells them apart: `evbk-main`, `evbk-background`, `evbk-listen`, `evbk-detacher`. `try_recv` returning `Ok(None)` means sqlx reconnected and notifications may be lost: the watch emits every subscribed partition's current head again (§4.7). An error reconnects with jittered exponential backoff, 1 s to 30 s.
- **Startup task.** `build` validates the settings and connects nothing. `start` starts one task and returns at once; it never fails because the database is unreachable (feature 0007 §2.7). The task, retrying with backoff until it succeeds or the backend is stopped:
  1. connects the pool, checks `server_version_num >= 140000` and `transaction_isolation = read committed`;
  2. `CREATE SCHEMA IF NOT EXISTS {schema}`;
  3. runs the embedded migrations (`sqlx::migrate!`, §3.2);
  4. connects the `LISTEN` connection, and under `head_notify: after_commit` starts the notifier;
  5. where the role runs it, makes the background pool and starts the maintenance workers (§4.9).
- **PgBouncer.** A transaction-mode pooler cannot be detected from a client: it would silently detach the `LISTEN` session. As in the cluster gear's PostgreSQL plugin, the operator declares it with `pgbouncer_transaction_mode: true`, and `build` refuses that with `InvalidConfig`.
  Until it completes, every operation returns `Unavailable` and readiness reports the entry as not ready.
- **Stop.** `stop` ends the startup task, the listener and the maintenance workers and waits for them, then closes the pool; every operation after it is `Unavailable`. Both waits give up once the gear's deadline is cancelled. The tasks run on a `TaskTracker` per instance, so `stop` waits for what the startup task started too. A worker's pass still running at `stop` is dropped after `toolkit-taskward`'s stop grace; its transaction rolls back, and a detach cut off there is finished by the next start (§4.9).

### 3.2 Migrations

- The plugin's own tables (`evbk__log`, `evbk__partition`, `evbk__retention`, `evbk__fence`, `evbk__segment`) come from SQL files embedded with `sqlx::migrate!("./src/migrations")`, run by the startup task. sqlx serialises concurrent runs with its own advisory lock and records versions in `_sqlx_migrations`.
- The files use no `IF NOT EXISTS`: a schema that drifted from the recorded version fails the startup task loudly rather than being patched over.
- The plugin keeps its objects in its own schema, `evbk` by default, so its `_sqlx_migrations` never meets another sqlx user's (the cluster plugin numbers its migrations from `0001` too). Table names keep the `evbk__` prefix as well (`docs/arch/database/ADR/0001`).
- Event tables are not migrations: `provision` creates them and the first segments (§4.1), the extender the rest (§4.9).

### 3.3 Lock Order

Every transaction that takes several row locks takes them in this order, so two appends never deadlock:

```
1. evbk__fence (fence_key)
2. evbk__log (topic_uuid)                      -- provision only
3. evbk__partition rows, ascending partition
```

No maintenance job takes an `evbk__partition` row lock, so none ever waits on an append or makes one wait. Each holds one lock per partition, and none of them is a lock `append` takes:

| Job | Lock | Held |
|---|---|---|
| extender | `pg_try_advisory_xact_lock(hashtextextended('evbk:seg:{t}:{p}', 0))` | until its transaction ends |
| reconcile | `evbk:seg:{t}:{p}`, then `evbk:det:{t}:{p}`, both `pg_try_advisory_xact_lock` | until its transaction ends |
| sealer | its full open `evbk__segment` rows, `FOR UPDATE SKIP LOCKED` | until its transaction ends |
| retention | its `evbk__retention` row, `FOR UPDATE NOWAIT` | until its transaction ends |
| detacher | `pg_try_advisory_lock(hashtextextended('evbk:det:{t}:{p}', 0))` | one step, on its own connection |

None of these waits: each is a try-lock, `SKIP LOCKED` or `NOWAIT`, so the two that reconcile takes cannot deadlock with anyone. Every lock is released on every path. A run that finishes, early or not, commits. A run that fails or is cancelled drops its transaction: sqlx queues the `ROLLBACK` and sends it when the pool pings the returned connection, or closes a connection that fails the ping. The detacher unlocks after each step, and on any error, panic or cancellation drops its connection, which ends the session and its locks.

`ATTACH PARTITION` and `DETACH ... CONCURRENTLY` take `SHARE UPDATE EXCLUSIVE` on the parent, which appends and reads do not conflict with, but which conflicts with itself: a detach waiting out old transactions holds it, and an `ATTACH` waits behind. The detacher bounds that wait at 2 s per step, below the extender's longest `lock_timeout` (5 s).

## 4. Operations

### 4.1 provision

`provision(TopicSpec { topic, partitions }, LogClaim { replacing, new })`:

```sql
BEGIN;
INSERT INTO evbk__log (topic_uuid, topic_id, log_id, partitions, max_age, max_bytes, provisioned_at)
VALUES ($t, $topic_id, $new, $partitions, $max_age, $max_bytes, now())
ON CONFLICT (topic_uuid) DO NOTHING;
SELECT log_id, partitions FROM evbk__log WHERE topic_uuid = $t FOR UPDATE;
--   log_id = $new                         -> this attempt (fresh, or continued); UPDATE max_age, max_bytes (re-provision with new settings)
--   log_id = replacing (Log(id))          -> UPDATE evbk__log SET log_id = $new WHERE topic_uuid = $t
--   anything else                         -> ROLLBACK; LogConflict
SELECT partition, table_name FROM evbk__partition WHERE topic_uuid = $t;   -- names already stored
-- per partition p in 0..partitions; parent = the stored name, else evbk__{t}_p{p} (§2.2):
SELECT to_regclass(parent) IS NULL;               -- a new table?
CREATE TABLE IF NOT EXISTS parent (...) PARTITION BY RANGE (sequence);
INSERT INTO evbk__partition (topic_uuid, partition, table_name) VALUES ($t, p, parent) ON CONFLICT DO NOTHING;
INSERT INTO evbk__retention (topic_uuid, partition) VALUES ($t, p) ON CONFLICT DO NOTHING;
-- only for a new table: its time index (§2.2), and the first segments, from 1, each with its open row (§4.9)
CREATE INDEX idx_parent__time ON parent USING brin (sequence_time);
CREATE TABLE parent_s0000000000000001 PARTITION OF parent FOR VALUES FROM (1) TO (1 + span);
INSERT INTO evbk__segment (..., first_seq, end_seq, table_name, state, changed_at)
VALUES (..., 1, 1 + span, 'parent_s0000000000000001', 'open', now());
-- and more while they leave less room than one append
COMMIT;
-> Ready { partitions: evbk__log.partitions }
```

After the commit, the new partitions' layouts go into the maintenance registry (§4.9).

- DDL is transactional in PostgreSQL: a failed provisioning leaves nothing behind.
- `partitions > 9999` is refused with `InvalidConfig` (naming, §2.2).
- `evbk__log.partitions` is the count reported; a different count requested later is the gear's concern (feature 0006 §4.1).
- A table that exists keeps its segments; a re-provision changes the bounds only.
- A re-provision takes no lock on an existing table: `CREATE TABLE IF NOT EXISTS` skips it unlocked, and `CREATE INDEX`, which takes `SHARE` on the table and every segment even when the index exists, runs only for a new one. An append never waits for a re-provision; a re-provision waits for an append in progress at most until it commits, on the partition's `evbk__partition` row.

### 4.2 append

`AppendRequest { fence: { key, seq }, topic, events, flush }`, here one event whose key maps to partition 3:

```sql
-- (1) provisioned?  (cached per topic; a miss reads evbk__log, before the transaction)
--   no row -> NotProvisioned, whatever the fence holds

BEGIN;                                                        -- READ COMMITTED
-- (2) fence: first statement; takes the fence key's row lock
INSERT INTO evbk__fence (fence_key, last_seq) VALUES ($key, $seq)
ON CONFLICT (fence_key)
DO UPDATE SET last_seq = EXCLUDED.last_seq
WHERE evbk__fence.last_seq < EXCLUDED.last_seq;
--   0 rows -> ROLLBACK; Ok(Duplicate)

-- (3) assign sequences: row lock on (topic, 3), held until COMMIT
UPDATE evbk__partition SET hwm = hwm + $n WHERE topic_uuid = $t AND partition = 3
RETURNING hwm, table_name, clock_timestamp();   -- the batch's last; events take the $n before it

-- (4) the events, sequence_time = that clock_timestamp() (taken after the row lock, so it rises with sequence);
--     into the stored table_name; PostgreSQL routes each row to its segment
INSERT INTO table_name (sequence, sequence_time, tenant_id, body)
VALUES (...);
--   no segment for a sequence (23514) -> ROLLBACK; Unavailable, and the extender is offered the partition at once (§4.9)

-- (5) wake watchers: every head of the append in one statement (§2.3); here under head_notify: in_transaction,
--     under after_commit the heads go to the notifier after the COMMIT instead
SELECT pg_notify('evbk__head', payload) FROM UNNEST(ARRAY[$t || ':3:' || $last]) AS payload;
COMMIT;
-> Ok(Heads([(topic, 3) -> last]))
```

Several partitions in one append repeat steps (3) and (4) per partition, in ascending order; step (5) is one statement for all of them.

After the commit, each new head is checked against the maintenance registry in memory: one lookup and two compares, no query, no lock. A head past its partition's room alarm offers the partition to the extender; past its seal alarm, to the sealer (§4.9); one the registry does not know yet, to both. `append` never writes `evbk__segment` and does no byte arithmetic: sizes are the jobs' to measure.

### 4.3 Deduplication Cases

| Case | What happens | Result |
|---|---|---|
| Retry after an ambiguous commit (reply lost after `COMMIT`) | step (2): `seq < seq` is false, 0 rows | `Duplicate`, nothing written |
| Dropped call still open, retry arrives | the retry's step (2) blocks on the fence row lock. If the old transaction commits, the retry's `WHERE` is re-checked against the committed row: 0 rows, `Duplicate`. If its connection closes, PostgreSQL aborts it and the retry stores the message once | a late commit can never land after its retry |
| Old transaction hung (half-open socket) | the retry waits up to `lock_timeout`, fails, and is retried by the outbox; the old transaction ends on `idle_in_transaction_session_timeout` or TCP keepalive | stored once |
| Hard restart of ingest | the outbox re-delivers unacked messages; each meets step (2) | applied ones `Duplicate`, the rest stored |
| Hard restart of PostgreSQL | committed transactions are in the WAL, uncommitted ones are gone; fence, events and head always agree | consistent |
| Two outbox groups, or two deployments, writing one database | different fence keys, different rows | independent |

### 4.4 Concurrent Ingest Partitions and Visibility

```
ingest partition 5 (seq 1207): key A -> partition 3      ingest partition 9 (seq 330): key B -> partition 3
T1: fence (k5) -> evbk__partition (t,3) -> 42
T2: fence (k9) -> waits on evbk__partition (t,3)
T1 COMMIT  -> T2 gets 43, COMMIT
```

The partition row lock is held until commit, so 43 is never visible before 42: visibility follows sequence order (feature 0007 §2.2). Ingest partitions contend only on the backend partitions they share.

### 4.5 read

```sql
SELECT sequence, sequence_time, body FROM table_name
 WHERE sequence > GREATEST($offset, (SELECT floor FROM evbk__retention WHERE topic_uuid = $t AND partition = $p))
 ORDER BY sequence LIMIT $max_events;
```

The floor hides expired segments still attached until the detacher gets to them (§4.9).

Every segment above the offset stays in the plan, and the `LIMIT` stops in the first: the others are never executed. An upper bound would name the same rows, since sequences are gapless (§4.2), but would save nothing: a bound on the floor's subquery prunes only during execution, after every segment is set up, and only what the `LIMIT` already skips.

A plain range read: no filters (filtering is delivery's; a filtered read may come later). `Wait::For` is served by a short `LISTEN`-backed wait on the plugin's notification fan-in, never by holding a pooled connection.

### 4.6 resolve

```sql
-- Latest = HWM; Earliest = floor: the oldest stored - 1, or the HWM when nothing is stored
SELECT p.hwm, r.floor FROM evbk__partition p JOIN evbk__retention r USING (topic_uuid, partition)
 WHERE p.topic_uuid = $t AND p.partition = $p;
-- At(t) = (first stored event with sequence_time >= t) - 1, else Latest; storage time, never occurred_at.
--   Every event of a sealed segment is no later than its newest_at (sequence_time rises with sequence),
--   so the search starts past the last segment sealed before t; the brin index (§2.2) skips what is
--   left before it.
SELECT sequence - 1 FROM evbk__{t}_p{p} WHERE sequence_time >= $t AND sequence > GREATEST($floor,
  (SELECT COALESCE(MAX(end_seq - 1), 0) FROM evbk__segment
    WHERE topic_uuid = $t AND partition = $p AND state = 'sealed' AND newest_at < $t))
 ORDER BY sequence LIMIT 1;
-- Exact(n): valid iff Earliest <= n <= Latest, else OffsetOutOfRange
```

No `evbk__log` row: `NotProvisioned`.

### 4.7 watch

```
HeadWatch.watch(partitions):
  first  SELECT p.hwm FROM UNNEST($topics::uuid[], $partitions::int[]) WITH ORDINALITY AS w(topic_uuid, partition, n)
           JOIN evbk__partition p USING (topic_uuid, partition) ORDER BY w.n      -- one query for every named partition
  then   every evbk__head notification for a named partition -> Head
  loss   reconnect, LISTEN, publish every partition's current head again; a watch passes on only heads
         above what it has, so a lost notification arrives late, never as an error item
```

### 4.8 query

Segment introspection (feature 0003) reads the segment list from the catalog and the figures from the tables:

```sql
SELECT c.relname, pg_get_expr(c.relpartbound, c.oid)
  FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid
 WHERE i.inhparent = 'evbk__{t}_p{p}'::regclass;
```

### 4.9 Maintenance

Background work keeps every partition appendable and within its topic's bounds: segments are made ahead of the head, sealed with their size once full, expired whole, then detached and dropped. A cross-check finds drift between the segment rows and the catalog. All of it runs on `toolkit-taskward` workers started after startup (§3.1), in `src/maintenance/`, and only in processes that append: `ingest` and `standalone`. A delivery process holds none of its connections and takes none of its locks; several ingest processes keep apart through the locks of §3.3.

**A segment's life.** Each state change and the work around it is one transaction, except for the detacher's (see below):

```
           extender            sealer                retention            detacher                 detacher
(none) ───────────► open ───────────────► sealed ───────────► expired ───────────► detaching ───────────► (table dropped,
       CREATE+ATTACH       hwm reaches end_seq - 1;  floor moved          DETACH CONCURRENTLY        row deleted)
       + INSERT row        bytes = table size        past it              (outside a transaction)   DROP + DELETE
```

- A segment table and its row are created together (extender, `provision`) and removed together (detacher). Only a `detaching` row may disagree with the catalog, while the detach is under way.
- `floor` moves only in a retention transaction, together with the segments it passes becoming `expired`.
- Only sealed segments expire: the segment being written to is never removed.

**Workers.** `start::spawn` builds every worker in one function; each is a `WorkerAction` whose `execute` is one whole pass:

| Worker | Count | Woken by | Does, per pass |
|---|---|---|---|
| `evbk-extender-{i}` | `extender` | its queue; once at start | makes the segments one partition lacks |
| `evbk-sealer-{i}` | `sealer` | its queue; once at start | seals one partition's full open segments with their size and newest time |
| `evbk-retention-{i}` | `retention` | its queue; once at start | expires one partition's sealed segments its bound no longer keeps |
| `evbk-detacher-{i}` | `detacher` | its queue; once at start | one detach or drop step on one partition |
| `evbk-reconcile-{i}` | `reconcile` | its queue | repairs one partition's drift |
| `evbk-rescan` | 1 | a timer every `rescan_interval`; once at start | rebuilds the registry, offers work, cross-checks |

```
append (after commit) ── registry check ──► extend / seal
append (23514) ─────────────────────────► extend (urgent)
provision (after commit) ───────────────► registry entries for new partitions
rescan ─► registry ; extend, seal, retain, detach ; every crosscheck_every-th pass: cross-check ─► detach / reconcile
extender ─► registry          sealer ─► registry ; retain, when it sealed a segment of a bounded topic
retention ─► detach           reconcile ─► registry
```

**Registry.** In memory, per partition: its open and sealed ranges, the head at which room gets short (`room_alarm`: the head from which fewer than two segments lie wholly above it, or less than one append of room is left), and the head at which the oldest open segment is full and due a seal (`seal_alarm`: its `end_seq - 1`). Nothing about sizes: no append knows one. `append` reads it under a read lock; each job replaces the entry of the partition it changed, from what it read in its own transaction, and the rescan replaces every entry its snapshot read. It keeps one it did not read: its snapshot may predate a partition provisioned since, and a partition that is gone is forgotten by the job that finds it so. The registry may lag the database, never lead it: a stale entry only asks for work that is already done, and the job finds that out.

**Queues.** Each job has a queue of partitions (`Prioritizer`): highest priority first, ties in the order offered. The extender's priority is how little room is left (no segment or no layout first); the other queues are first come, first served.

- Every offer bumps the partition's version, and a lease remembers the version it took. Offers, takes and settles run under one mutex, so an offer during a run, even after its commit, keeps the partition pending: no offer is lost.
- A run settles its lease by what it found: done (dropped unless offered meanwhile), again (back at once), busy (another instance holds the partition, which finishes it), gone (the partition no longer exists), contended (back after 250 ms), failed (back after 100 ms, doubling to 30 s), deferred (the database is down: back at once, uncounted, and the worker backs off, 100 ms doubling to 30 s, until a pass succeeds). A dropped lease puts the partition back.
- A partition postponed `warn_after` times in a row is logged once as a warning; after `stuck_after` postponements, or `stuck_age` of them, it is logged once as stuck and retried every 30 s; its next success logs that it recovered.
- A run's `lock_timeout` grows with the postponements it follows: 500 ms, 1 s, 2 s, then 5 s.

**Extender** (`evbk:seg` advisory lock, one transaction):

```sql
SET LOCAL lock_timeout = ...; SET LOCAL statement_timeout = '10s';
SELECT pg_try_advisory_xact_lock(hashtextextended('evbk:seg:{t}:{p}', 0));   -- false -> busy
SELECT hwm, table_name FROM evbk__partition ...;                              -- none -> gone
SELECT first_seq, end_seq, state, bytes, newest_at FROM evbk__segment
 WHERE ... AND state IN ('open', 'sealed') ORDER BY first_seq;
-- per range the head lacks, `span` sequences each (§5 `segment`):
CREATE TABLE parent_s{first} (LIKE parent INCLUDING ALL);
ALTER TABLE parent ATTACH PARTITION parent_s{first} FOR VALUES FROM (first) TO (end);
INSERT INTO evbk__segment (...) VALUES (..., 'open', now());
COMMIT;
```

A `42P07` (the name is taken) or `42P17` (the range overlaps) means a table no row names is in the way; the extender offers the partition to reconcile at once, which adopts an attached one (cross-check C7) under its locks, and the extender's retry continues from it.

`ATTACH` of an empty table takes `SHARE UPDATE EXCLUSIVE` on the parent; `CREATE ... PARTITION OF` would take `ACCESS EXCLUSIVE` and stop appends and reads. In `period` mode the span comes from the rate between the ends of the two newest sealed segments (`end_seq - 1` and `newest_at`), with no further query.

**Sealer** (`SKIP LOCKED` row locks, one transaction):

```sql
SELECT p.hwm, <bounded> FROM evbk__partition p ...;                            -- none -> gone
SELECT first_seq, table_name FROM evbk__segment
 WHERE ... AND state = 'open' AND end_seq - 1 <= $hwm ORDER BY first_seq FOR UPDATE SKIP LOCKED;   -- the full ones
-- per full segment, on its own table: a file-size read and one primary-key probe, no scan
SELECT pg_total_relation_size('{segment}'::regclass),
       (SELECT sequence_time FROM {segment} ORDER BY sequence DESC LIMIT 1);
UPDATE evbk__segment SET state = 'sealed', bytes = <size>, newest_at = <time> ...;
COMMIT;                                                -- sealed on a bounded topic -> offers retention
```

A segment is full once the head is at or past its last sequence: an append commits its rows and its `hwm` together, and sequences have no gaps, so every row of it is visible and none is still to come. `append` never locks these rows, and a full segment takes no more inserts, so the two never wait on each other. The sealer runs when a partition's oldest open segment fills (`seal_alarm`), from the append that filled it or the rescan; an open segment has no size and no newest time until then. Events deleted by hand (§4.10) leave a table as large as what stays, and one with none left a `NULL` time.

**Retention** (`NOWAIT` row lock, one transaction):

```sql
SET LOCAL lock_timeout = ...;
SELECT <by age>, l.max_bytes FROM evbk__retention r ... FOR UPDATE OF r NOWAIT;   -- 55P03 -> busy; none -> gone
-- with a byte bound: the partition's size on disk (§2.2)
SELECT SUM(CASE WHEN state = 'open' THEN pg_total_relation_size(to_regclass(table_name)) ELSE bytes END)
  FROM evbk__segment WHERE ... AND state IN ('open', 'sealed');
SELECT first_seq, end_seq, bytes, newest_at IS NULL OR newest_at < now() - max_age FROM evbk__segment
 WHERE ... AND state = 'sealed' ORDER BY first_seq LIMIT 64;
-- oldest first, while aged, or while what stays still holds max_bytes without it; then stop
UPDATE evbk__retention SET floor = <last due end_seq - 1> ...;
UPDATE evbk__segment SET state = 'expired' ... WHERE first_seq = ANY(<due>);
COMMIT;                                                -- then offers the detacher; 64 due -> again
```

A segment sealed with no event left (§4.10) is past any age.

No DDL: readers stop resolving into expired segments at this commit, since `resolve`, `read` and `query` read nothing at or below the floor (§4.5, §4.6). By bytes, a segment goes only while what stays still holds at least `max_bytes`, as in Kafka.

**Detacher** (`evbk:det` session lock, its own connection, one step per pass). `DETACH ... CONCURRENTLY` refuses a transaction block, so this is the one job that is not one transaction; the row records each step, so a step cut off anywhere is resumed from the row and the catalog. Its session runs with `statement_timeout = 2s`, so a detach waiting on a long transaction is cut off and left pending, holding no lock until the next pass finalizes it, and `client_connection_check_interval = 1s`, so a detach whose client is gone ends with it:

```
pg_try_advisory_lock('evbk:det:{t}:{p}')                                    -- false -> busy
s = the oldest row of the partition in ('expired', 'detaching'), with the parent's table_name    -- none -> done
pending = pg_inherits.inhdetachpending of s's table; none = not attached
  (expired,   attached, not pending) -> UPDATE state = 'detaching'; DETACH PARTITION ... CONCURRENTLY
  (any,       attached, not pending) -> DETACH PARTITION ... CONCURRENTLY      -- cut off before it began
  (any,       attached, pending)     -> DETACH PARTITION ... FINALIZE          -- cut off while waiting
  (any,       not attached)          -> BEGIN; DROP TABLE IF EXISTS; DELETE the row; COMMIT
pg_advisory_unlock; again                     -- after an error the connection is dropped instead
```

**Rescan.** Every `rescan_interval`, and once at start, one `REPEATABLE READ, READ ONLY` transaction reads every partition (`hwm`, `floor`, bounds) and every segment row. It replaces their registry entries, then offers:

- the extender every partition that needs room;
- the sealer every partition past its `seal_alarm`;
- retention every partition with a byte bound and a sealed segment, since only retention measures the size, and every partition whose oldest sealed segment's `newest_at` is older than `max_age` or `NULL`;
- the detacher every partition with an `expired` or `detaching` row.

It catches what the in-memory checks miss: a restart, another instance's appends, a lost offer.

**Cross-check.** Every `crosscheck_every` rescans, the first included, the rescan compares the segment rows with the tables the catalog holds: those the rows name, and those named by the rules of §2.2 (`^evbk__[0-9a-f]{32}_p[0-9]+_s[0-9a-f]{16}$`), which places a table that has no row. A detached table has no bound left: the next recorded segment's `first_seq` ends its range.

| Case | Row | Table | Reaction |
|---|---|---|---|
| C1 | open, sealed or expired | attached, bound matches | none |
| C2 | detaching | any | none: the detacher's own steps |
| C3 | expired | detached or missing | the detacher |
| C4 | open, `first_seq > hwm` | missing | reconcile deletes the row; the extender makes both again |
| C5 | open or sealed, `first_seq <= hwm` | missing | reported: events lost; reconcile keeps failing on it until it is stuck |
| C6 | open or sealed | detached, or its detach pending | reported: events hidden |
| C7 | none | attached | reconcile adopts it: an `open` row, not measured |
| C8 | none | detached, wholly at or below the floor | reconcile drops it |
| C8b | none | detached, otherwise | reported |
| C9 | any but detaching | attached, bound differs from the row | reported |
| C10 | the open and sealed rows leave a gap above the floor | - | reported; reconcile keeps failing on it until it is stuck |

- The catalog is not read in the rows' snapshot, so the rescan acts on a finding only once two cross-checks in a row found it, logs it once as an error, and logs once more when it clears.
- Repairs never run in the rescan. Reconcile takes the partition's `evbk:seg` and `evbk:det` locks, so neither the extender nor the detacher changes a segment meanwhile, reads the rows and the catalog again, and repairs only what still holds.

**Errors.** A failed run is classified by `sqlx` error kind and SQLSTATE; the worker settles its lease from the class:

| Class | Errors | Settles | Logged |
|---|---|---|---|
| Medium | I/O, TLS, protocol, `08*`, pool timed out or closed, `57P01`-`57P03`, `53300`, `25006` | deferred; the worker backs off | by the worker's listener |
| Contended | `55P03`, `40P01`, `40001` | contended | debug |
| Gone | `42P01` | gone; the registry forgets the partition | - |
| Stale | `42P07`, `42P17` on a partition's first attempt | contended | debug |
| Failed | `57014` | failed | debug |
| Failed | `25P03`, `53200`, `54001`, anything else | failed | warn |
| Failed | `53100` | failed | error |
| Bug | `23514`; `42P07`, `42P17` on a later attempt | failed | error |
| Permanent | `42501`, `3F000`, `3D000`, `28*` | failed | error |

Retention reads `55P03` on its `NOWAIT` lock as busy before any of this.

**Several instances.** Every instance runs every worker. The locks of §3.3 keep two instances off one partition: the second one's run answers busy and drops the partition, and the holder finishes the work. Offers are per instance; the rescan of each covers what the others did.

### 4.10 Route Changes, Foreign Logs, Lost Data

| Case | What the plugin sees | Result |
|---|---|---|
| A provisioned topic routed here by a routing change | no `evbk__log` row | `NotProvisioned` |
| The database holds a different log id for the topic | `provision` finds a foreign `evbk__log.log_id` | `LogConflict` |
| The database restored from an older backup | `evbk__fence.last_seq` and `hwm` regress together with the data | events after the backup are lost; new appends continue from the restored head (ADR-0008 O5) |
| Events deleted from a segment by hand | a gap in `(floor, hwm]`; `hwm` and the fence rows unchanged | lost data. `read` returns the events after the gap with no error, and `resolve(Exact)` accepts a sequence inside it. Appends go on; the segment still seals when full, with its table's size and its last remaining event's time (`NULL` with none left, which counts as past any age), and still expires. The storage invariants report it (L2, and L5 for a segment sealed before); the plugin does not |

## 5. Configuration

```yaml
backends:
  pg-main:
    type: "gts.cf.core.events.backend.v1~cf.core.backend.postgres.v1~"
    url: "postgres://eb:${EB_PG_PASSWORD}@pg-1:5432/events"   # required; ${VAR} / ${VAR:-default} expanded
    schema: "evbk"                     # every object of this plugin; created if absent. Default "evbk"
    pools:
      main: 10                         # every operation. Default 10
      background: 6                    # the maintenance workers, where they run (§3.1). Default and minimum:
                                       # extender + sealer + retention + reconcile concurrency + 1
    pool_acquire_timeout_ms: 5000      # how long an operation waits for a pooled connection. Default 5000
    segment: { events: 1000000 }       # or { period: "PT1H" }; how the next segment is sized. Default events 1000000
    head_notify: in_transaction        # or after_commit: who sends an append's head notification (§2.3). Default in_transaction
    pgbouncer_transaction_mode: false  # true is refused (§3.1). Default false
    maintenance:                       # the background workers (§4.9); every field optional
      rescan_interval_ms: 60000        # between two rescans. Default 60000
      crosscheck_every: 10             # rescans per cross-check. Default 10
      extender:  { concurrency: 2 }    # workers, one pooled connection each. Default 2
      sealer:    { concurrency: 1 }    # pooled. Default 1
      retention: { concurrency: 1 }    # pooled. Default 1
      reconcile: { concurrency: 1 }    # pooled. Default 1
      detacher:  { concurrency: 1 }    # one connection each, outside the pool. Default 1
      postponed:                       # when a partition a job keeps postponing is reported
        warn_after: 5                  # postponements in a row before a warning. Default 5
        stuck_after: 20                # postponements in a row before it is stuck. Default 20
        stuck_age_ms: 300000           # or this long postponed. Default 300000
```

- Settings are read by hand, not through serde, so an error names the setting and the rule it breaks and never quotes a value (feature 0007 §2.7). An unknown setting is refused.
- `schema` must be a bare identifier (`[a-z_][a-z0-9_]*`) of at most 63 bytes; it is interpolated unquoted.
- `Debug` prints `url` as `<redacted>`.
- Every count and interval is a positive integer. `pools.background` must be at least `extender + sealer + retention + reconcile + 1` (the rescan's): a worker waiting for a connection would time out, and that reads as the database being down.
- The settings are the same in every process (feature 0005); the role decides which of them apply: a delivery process reads neither `pools.background` nor `maintenance`.

## 6. Error Mapping

| Situation | `BackendError` |
|---|---|
| Connection refused, I/O, TLS, protocol, pool closed, pool timeout, SQLSTATE class `08`, `lock_timeout` (`55P03`), statement timeout (`57014`), serialization failure (`40001`), deadlock (`40P01`), too many connections (`53300`), server shutting down or starting (`57P01`-`57P03`), startup task not yet complete | `Unavailable` |
| SQLSTATE `28*` (authentication) | `Unavailable`, logged as an authentication failure |
| No segment for a sequence (`23514`) | `Unavailable` |
| No `evbk__log` row | `NotProvisioned` |
| Foreign log id at provisioning | `LogConflict` |
| `Exact(n)` outside `[Earliest, Latest]` | `OffsetOutOfRange` |
| Unknown or invalid setting | `InvalidConfig` |
| Any other SQLSTATE in `append` | `AppendFailed` |
| Any other SQLSTATE in `read`, `resolve`, `watch` or `query` | `ReadFailed` |
| Any other SQLSTATE in `provision` | `Internal` |
| A driver failure (decode, encode, missing row or column, configuration) | `Internal` |

A message names the operation and the SQLSTATE, or the kind of driver failure, and nothing else: PostgreSQL's message and `DETAIL`, and the driver's own text, may quote stored values.

## 7. Risks / Trade-offs

- **One row lock per partition per append.** Appends to one partition serialize on its `evbk__partition` row: the price of gap-free, in-order visibility. A hot partition is bounded by one commit round trip per append.
- **Catalog size.** Tables = topics x partitions x live segments. Many topics with long retention and small spans make a large catalog; `period` mode or a larger `events` span keeps it in check.
- **Long retention.** A ten-year window at high rates exceeds what one PostgreSQL holds; tiering is §9.
- **Whole segments only.** Retention removes sealed segments, never the one being written to: an idle partition keeps up to one open segment of events past its bounds, and a partition bound by bytes keeps between `max_bytes` and `max_bytes` plus its oldest sealed segment.
- **`max_bytes` is disk, measured late.** The size is what the tables take on disk, indexes and the empty segments ahead included, so a small `max_bytes` is mostly overhead (a few tens of KB per segment). Only retention measures it, woken by a seal or a rescan: a partition can stay over its byte bound for up to `rescan_interval`.
- **Notifying commits run one at a time.** Under `head_notify: in_transaction` every append's commit takes its turn at the cluster's notify lock, held across the commit flush: appends to different partitions, even different topics, serialize there. `after_commit` moves the wait to one notifier per instance, at the price below.
- **A notification lost after its commit.** Under `head_notify: after_commit`, a crash between an append's commit and the notifier's send loses that head's notification: another instance's watch of the partition sees the head with the partition's next append, or when its `LISTEN` connection reconnects. Readers that poll the tail and waiting reads (re-read every second) do not depend on it.
- **Room is made ahead, not on demand.** An append that outruns the extender fails with `Unavailable` and is retried by the outbox; the failed insert offers the partition to the extender at once.
- **What stops progress.** Permanent errors (privileges, a missing schema or database, credentials), a full disk, a persistent failover to a read-only standby, and a bug that repeats on one partition. Each leaves the partitions it touches stuck and logged once (§4.9); once a partition's room runs out, its appends fail with `Unavailable`.
- **One `ACCESS EXCLUSIVE` lock.** `DROP TABLE` of a detached segment, on a table no reader resolves into. Every other DDL takes `SHARE UPDATE EXCLUSIVE` on the parent.
- **The registry can briefly show a deleted segment.** Each job writes its partition's registry entry after its commit, and the rescan replaces every entry from a snapshot taken before; a replace from older data can land after one from newer data. Usually that only makes the registry lag, showing less room than exists. After reconcile deletes a row (cross-check C4), a rescan from before that delete shows the deleted segment again until its next pass; an append can then run out of room unwarned, gets `23514`, and offers the extender itself.
- **A stray detached table can block the extender.** An attached table with no row in the extender's way is adopted at once (§4.9). A detached one above the floor (cross-check C8b, reported only) is not: every extender run fails with `42P07`, the partition ends stuck and is retried every 30 s, and its appends fail once its room runs out. Only an operator can tell whether such a table holds events worth keeping; once it is dropped or renamed, the next retry succeeds.
- **A gap in the middle stays.** The extender adds segments only past the last one. A row missing below other live rows (cross-check C4 between them, C10) is reported and left stuck, not repaired.
- **No metrics.** The plugin exports none, and the backend interface has no readiness signal it can feed: stuck partitions are visible in the logs only.

## 8. Open Questions

- Ingest database restored from backup: a fence key returns with a lower `seq` than `evbk__fence` holds, so new messages are dropped as duplicates (feature 0007 §6).

## 9. Later

- A room margin from each partition's append rate, so busy partitions get segments earlier; today the margin is a constant 0.
- Per-segment figures (`bytes`, `newest_at`) in `query`'s segment entries.
- A faster wake-up for byte retention, should `rescan_interval` prove too slow: an estimate kept by `append`, or a size check on each seal of any segment.
- A gauge of stuck partitions per job, once the plugin exports metrics.

- PostgreSQL partitioning by time inside a segment (sub-partitioning by `sequence_time`) if a retention bound needs finer drops.
- Tiering: detach old segments and move them to cheaper storage or an object store; a detached segment is still identified by its name.
- A log-id check on every append if ADR-0008 O1(c) is adopted: step (1) of §4.2 becomes `SELECT 1 FROM evbk__log WHERE topic_uuid = $t AND log_id = $expected`.
