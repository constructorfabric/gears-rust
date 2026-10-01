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
  - [4.9 Maintenance Worker: Segments and Retention](#49-maintenance-worker-segments-and-retention)
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

CREATE TABLE evbk__partition (                        -- one row per (topic, partition); written by append only
    topic_uuid     UUID    NOT NULL REFERENCES evbk__log,
    partition      INTEGER NOT NULL,
    last_sequence  BIGINT  NOT NULL DEFAULT 0,     -- HWM: highest ever assigned; 0 = nothing yet
    appended_bytes BIGINT  NOT NULL DEFAULT 0,     -- cumulative, never lowered
    PRIMARY KEY (topic_uuid, partition)
);

CREATE TABLE evbk__retention (                        -- one row per (topic, partition); written by maintenance only
    topic_uuid     UUID    NOT NULL,
    partition      INTEGER NOT NULL,
    floor          BIGINT  NOT NULL DEFAULT 0,     -- highest removed sequence; stored = (floor, last_sequence]
    removed_bytes  BIGINT  NOT NULL DEFAULT 0,     -- cumulative, never lowered
    PRIMARY KEY (topic_uuid, partition),
    FOREIGN KEY (topic_uuid, partition) REFERENCES evbk__partition
);

CREATE TABLE evbk__fence (                            -- deduplication, one row per fence key
    fence_key         UUID     PRIMARY KEY,        -- FenceKey's 16 bytes, opaque to the plugin
    last_seq          BIGINT   NOT NULL            -- last applied FenceSeq
);
```

Each counter row has one kind of writer. `append` holds the `evbk__partition` row lock until commit (§4.4); were retention to write the same row, an append would wait out every expiry `DELETE` and segment drop, and fail on `lock_timeout`. Stored figures are derived, in one snapshot of both rows:

```
stored_events = last_sequence  - floor          -- sequences are gap-free and retention removes a prefix
stored_bytes  = appended_bytes - removed_bytes
```

Native PostgreSQL types: this database belongs to the plugin, so the gear's portable-types rule does not apply. `evbk__fence` holds one row per fence key the caller uses - a handful; nothing is indexed per event for deduplication.

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

Every other object follows the same ADR: indexes `idx_<table>__<purpose>`, e.g. `idx_evbk__partition__topic`. The prefix stays even in a dedicated database: an entry's `url` may point at a database other objects share.

```sql
CREATE TABLE evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3 (
    sequence       BIGINT        NOT NULL,         -- from 1 (ADR-0001)
    sequence_time  TIMESTAMPTZ   NOT NULL,         -- when it was appended; retention age
    occurred_at    TIMESTAMPTZ   NOT NULL,         -- resolve(At); never retention (the producer's claim)
    type_id        VARCHAR(1024) NOT NULL,         -- subscription `types` filter
    subject_type   VARCHAR(1024) NOT NULL,         -- subscription `subject_types` filter
    tenant_id      UUID          NOT NULL,         -- tenant scoping
    stored_bytes   INTEGER       NOT NULL,         -- byte retention
    body           BYTEA         NOT NULL,         -- the event, encoded as below; opaque to PostgreSQL
    PRIMARY KEY (sequence)
) PARTITION BY RANGE (sequence);

CREATE INDEX idx_evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3__occurred ON evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3 (occurred_at);
-- 4 + 44 + 10 = 58 <= 63 at 4-digit partitions; the per-segment copies PostgreSQL creates are backend-named, outside the ADR's grammar

CREATE TABLE evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3_s0000000000000001
    PARTITION OF evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3 FOR VALUES FROM (1) TO (1000001);
```

- Names are derived from `(topic_uuid, partition, first_sequence)`; nothing stores them.
- Only the fields a read filters or a retention pass measures are columns; everything else of the event lives in `body`.
- `body` is the plugin's own versioned encoding, JSON bytes: `{"v":1, "id", "type_id", "tenant_id", "source", "subject", "subject_type", "occurred_at", "trace_parent", "data"}`. `meta` is not stored (publisher-only, stripped on read); `partition`, `sequence` and `sequence_time` are columns. A reader refuses a `v` it does not know with `Internal`.
- `stored_bytes` is the length of `body`.
- A segment's range is fixed when it is created (§4.9); sizes may differ from segment to segment, and the suffix keeps every name exact.

### 2.3 NOTIFY Payload

```
channel  evbk__head
payload  "{topic_uuid hex}:{partition}:{sequence}"     e.g. "3f2a...b7a3:3:42"
```

Sent inside the append transaction; PostgreSQL delivers it on commit, in commit order.

## 3. Component Model

### 3.1 Connections

The plugin talks to PostgreSQL through `sqlx` directly, following the cluster gear's Postgres plugin (`gears/system/cluster/plugins/postgres-cluster-plugin`): dynamic table names, transactional DDL, `LISTEN/NOTIFY` and pool connect hooks have no SeaORM equivalent. The crate allows the direct use and says why in its `lib.rs` documentation:

```rust
// Why `sqlx` directly, not `libs/toolkit-db`: dynamic per-topic tables, transactional DDL,
// LISTEN/NOTIFY on a session-pinned connection, and pool connect hooks.
#![allow(unknown_lints)]
#![allow(de0706_no_direct_sqlx)]
```

- **Pool.** `PgPoolOptions` with `max_connections = pool_max_size` and `acquire_timeout`; `after_connect` sets `synchronous_commit = on`, `search_path = {schema}`, `lock_timeout = 5s` and `idle_in_transaction_session_timeout = 30s` - the last two bound a retry waiting on a hung transaction's fence row (§4.3). Constants, not settings. Used by `append`, `read`, `resolve`, `query`, `provision` and the maintenance worker.
- **`LISTEN` connection.** A `PgListener` on its own connection outside the pool (`pool_max_size + 1` connections in all), held in `LISTEN evbk__head`. `try_recv` returning `Ok(None)` means sqlx reconnected and notifications may be lost: the watch emits every subscribed partition's current head again (§4.7). An error reconnects with jittered exponential backoff, 1 s to 30 s.
- **Startup task.** `build` validates the settings, starts one task and returns at once; it never fails because the database is unreachable (feature 0007 §2.7). The task, retrying with backoff until it succeeds or `stop` is cancelled:
  1. connects the pool, checks `server_version_num >= 140000` (`DETACH ... CONCURRENTLY`, §4.9) and `transaction_isolation = read committed`;
  2. `CREATE SCHEMA IF NOT EXISTS {schema}`;
  3. runs the embedded migrations (`sqlx::migrate!`, §3.2);
  4. connects the `LISTEN` connection and starts the maintenance worker (§4.9).
- **PgBouncer.** A transaction-mode pooler cannot be detected from a client: it would silently detach the `LISTEN` session. As in the cluster gear's PostgreSQL plugin, the operator declares it with `pgbouncer_transaction_mode: true`, and `build` refuses that with `InvalidConfig`.
  Until it completes, every operation returns `Unavailable` and readiness reports the entry as not ready.
- **Stop.** Cancelling `stop` ends the startup task, the listener and the maintenance worker; each finishes its current step, and the pool is closed with a 10 s timeout.

### 3.2 Migrations

- The plugin's own tables (`evbk__log`, `evbk__partition`, `evbk__retention`, `evbk__fence`) come from SQL files embedded with `sqlx::migrate!("./src/migrations")`, run by the startup task. sqlx serialises concurrent runs with its own advisory lock and records versions in `_sqlx_migrations`.
- The files use no `IF NOT EXISTS`: a schema that drifted from the recorded version fails the startup task loudly rather than being patched over.
- The plugin keeps its objects in its own schema, `evbk` by default, so its `_sqlx_migrations` never meets another sqlx user's (the cluster plugin numbers its migrations from `0001` too). Table names keep the `evbk__` prefix as well (`docs/arch/database/ADR/0001`).
- Segment tables are not migrations: `provision` and the maintenance worker create them (§4.1, §4.9).

### 3.3 Lock Order

Every transaction that takes several row locks takes them in this order, so two appends never deadlock:

```
1. evbk__fence (fence_key)
2. evbk__log (topic_uuid)                      -- provision and retention only
3. evbk__partition rows, ascending partition
```

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
-- per partition p in 0..partitions:
CREATE TABLE IF NOT EXISTS evbk__{t}_p{p} (...) PARTITION BY RANGE (sequence);
CREATE INDEX IF NOT EXISTS ... ON evbk__{t}_p{p} (occurred_at);
CREATE TABLE IF NOT EXISTS evbk__{t}_p{p}_s0000000000000001 PARTITION OF evbk__{t}_p{p}
    FOR VALUES FROM (1) TO (1 + span(segment));   -- the first segment's span, as every segment's (§4.9)
INSERT INTO evbk__partition (topic_uuid, partition) VALUES ($t, p) ON CONFLICT DO NOTHING;
INSERT INTO evbk__retention (topic_uuid, partition) VALUES ($t, p) ON CONFLICT DO NOTHING;
COMMIT;
-> Ready { partitions: evbk__log.partitions }
```

- DDL is transactional in PostgreSQL: a failed provisioning leaves nothing behind.
- `partitions > 9999` is refused with `InvalidConfig` (naming, §2.2).
- `evbk__log.partitions` is the count reported; a different count requested later is the gear's concern (feature 0006 §3.1).

### 4.2 append

`AppendRequest { fence: { key, seq }, topic, events, flush }`, here one event whose key maps to partition 3:

```sql
BEGIN;                                                        -- READ COMMITTED
-- (1) fence: first statement; takes the fence key's row lock
INSERT INTO evbk__fence (fence_key, last_seq) VALUES ($key, $seq)
ON CONFLICT (fence_key)
DO UPDATE SET last_seq = EXCLUDED.last_seq
WHERE evbk__fence.last_seq < EXCLUDED.last_seq;
--   0 rows -> ROLLBACK; Ok(Duplicate)

-- (2) provisioned?  (cached per topic; a miss reads evbk__log)
--   no row -> ROLLBACK; NotProvisioned

-- (3) assign sequences: row lock on (topic, 3), held until COMMIT
UPDATE evbk__partition
   SET last_sequence  = last_sequence + $n,
       appended_bytes = appended_bytes + $bytes
 WHERE topic_uuid = $t AND partition = 3
RETURNING last_sequence;                                      -- the batch's last; events take the $n before it

-- (4) the events, sequence_time = clock_timestamp() (taken after the row lock, so it rises with sequence);
--     PostgreSQL routes each row to its segment
INSERT INTO evbk__{t}_p3 (sequence, sequence_time, occurred_at, type_id, subject_type, tenant_id, stored_bytes, body)
VALUES (...);
--   no segment for a sequence -> error -> ROLLBACK; Unavailable (maintenance is behind, §4.9)

-- (5) wake watchers
SELECT pg_notify('evbk__head', $t || ':3:' || $last);
COMMIT;
-> Ok(Heads([(topic, 3) -> last]))
```

Several partitions in one append repeat steps (3)-(5) per partition, in ascending order.

### 4.3 Deduplication Cases

| Case | What happens | Result |
|---|---|---|
| Retry after an ambiguous commit (reply lost after `COMMIT`) | step (1): `seq < seq` is false, 0 rows | `Duplicate`, nothing written |
| Dropped call still open, retry arrives | the retry's step (1) blocks on the fence row lock. If the old transaction commits, the retry's `WHERE` is re-checked against the committed row: 0 rows, `Duplicate`. If its connection closes, PostgreSQL aborts it and the retry stores the message once | a late commit can never land after its retry |
| Old transaction hung (half-open socket) | the retry waits up to `lock_timeout`, fails, and is retried by the outbox; the old transaction ends on `idle_in_transaction_session_timeout` or TCP keepalive | stored once |
| Hard restart of ingest | the outbox re-delivers unacked messages; each meets step (1) | applied ones `Duplicate`, the rest stored |
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
SELECT sequence, sequence_time, body FROM evbk__{t}_p{p}
 WHERE sequence > $offset
 ORDER BY sequence LIMIT $max_events;
```

A plain range read: no filters (filtering is delivery's; a filtered read may come later). Pruning, at plan time or at run time for the bound `$offset`, touches only the segments above the offset. `Wait::For` is served by a short `LISTEN`-backed wait on the plugin's notification fan-in, never by holding a pooled connection.

### 4.6 resolve

```sql
-- Latest = HWM; Earliest = floor: the oldest stored - 1, or the HWM when nothing is stored
SELECT p.last_sequence, r.floor FROM evbk__partition p JOIN evbk__retention r USING (topic_uuid, partition)
 WHERE p.topic_uuid = $t AND p.partition = $p;
-- At(t) = (first stored event with occurred_at >= t) - 1, else Latest
SELECT sequence - 1 FROM evbk__{t}_p{p} WHERE occurred_at >= $t AND sequence > $floor
 ORDER BY occurred_at, sequence LIMIT 1;
-- Exact(n): valid iff Earliest <= n <= Latest, else OffsetOutOfRange
```

No `evbk__log` row: `NotProvisioned`.

### 4.7 watch

```
HeadWatch.watch(partitions):
  first  SELECT partition, last_sequence FROM evbk__partition WHERE topic_uuid = $t AND partition = ANY($ps)
  then   every evbk__head notification for a named partition -> Head
  loss   reconnect, LISTEN, emit the current heads again, as an error item first if the gap matters
```

### 4.8 query

Segment introspection (feature 0003) reads the segment list from the catalog and the figures from the tables:

```sql
SELECT c.relname, pg_get_expr(c.relpartbound, c.oid)
  FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid
 WHERE i.inhparent = 'evbk__{t}_p{p}'::regclass;
```

### 4.9 Maintenance Worker: Segments and Retention

The plugin's own worker, started by `build`; the gear never calls into it. Every process that builds the registry entry runs one, so each pass over a `(topic, partition)` first takes `pg_try_advisory_lock(hashtextextended('evbk:' || topic_uuid || ':' || partition, 0))` and skips the partition if another process holds it. The bounds come from `evbk__log.max_age` / `max_bytes`, written by `provision` (feature 0007 §2.6). Each pass over a `(topic, partition)`:

```
1. ahead:    once last_sequence passes half of the last segment's range (last_sequence >= first + span / 2):
               CREATE TABLE evbk__{t}_p{p}_s{next} (LIKE evbk__{t}_p{p} INCLUDING ALL);
               ALTER TABLE evbk__{t}_p{p} ATTACH PARTITION evbk__{t}_p{p}_s{next} FOR VALUES FROM (next) TO (next + span)
2. expire:   oldest_permitted = now() - max_age; for each segment but the last, from the oldest,
             while its whole range is assigned and its newest sequence_time < oldest_permitted:
               UPDATE evbk__retention SET removed_bytes += its bytes above floor, floor = segment_end - 1
                WHERE floor < segment_end - 1;                                   -- a retry counts nothing twice
               ALTER TABLE evbk__{t}_p{p} DETACH PARTITION segment CONCURRENTLY; DROP TABLE segment
3. boundary: DELETE FROM evbk__{t}_p{p} WHERE sequence > floor AND sequence_time < oldest_permitted
             and, with a byte bound, the oldest rows above floor until stored_bytes <= max_bytes;
             floor and removed_bytes advanced in the same statement
```

Segment span:

| `segment` setting | Span of the next segment |
|---|---|
| `events: N` (default `1000000`) | exactly `N` sequences |
| `period: P` (ISO 8601 duration) | `rate x P`, `rate` being the partition's events per second over the last segment's lifetime (its first to its last `sequence_time`), floored at 1 event/s; a segment covers about `P`, more or less under traffic changes |

- **Cadence.** A pass every `maintenance_interval` (default 10 s), an immediate pass after `provision`, and one whenever an append observes `last_sequence` crossing the half-way point of its segment. A segment is created once half of the current one is used, so a partition has half a segment of room before an append could find no segment; if it does, `append` fails with `Unavailable` and the outbox retries.
- **No stall on DDL.** `ATTACH PARTITION` and `DETACH PARTITION ... CONCURRENTLY` take a lock that does not block reads or writes of the partitioned table, unlike `CREATE TABLE ... PARTITION OF` and `DROP TABLE` on an attached partition, which take `ACCESS EXCLUSIVE` on the parent. `DETACH ... CONCURRENTLY` runs outside a transaction block, so the pass holds `pg_try_advisory_lock` (session) on a dedicated pooled connection rather than the transaction-scoped one.
- The worker runs whether or not the topic has a retention bound: creating space ahead of the head is part of it.
- On `stop` the worker finishes the partition it holds and exits.
- `last_sequence` is never lowered: dropping or deleting events never moves the HWM.
- Retention writes `evbk__retention` only, never `evbk__partition`, so it never waits on an append's row lock nor makes one wait (§2.1).
- The floor moves before a segment is detached: rows at or below the floor are already removed for `resolve`, and the boundary steps skip them, while their segment waits to be dropped.
- Retention by time is accurate to one segment; the boundary `DELETE` makes it exact.
- Age is `sequence_time`, never `occurred_at`: the producer sets `occurred_at` and may set any time, so retention by it would delete an event on arrival or keep it forever. `sequence_time` is `clock_timestamp()` taken after the partition row lock in §4.2 step (3), so it rises with `sequence` within a partition and the expired events are always a prefix.

### 4.10 Route Changes, Foreign Logs, Lost Data

| Case | What the plugin sees | Result |
|---|---|---|
| A provisioned topic routed here by a routing change | no `evbk__log` row | `NotProvisioned` |
| The database holds a different log id for the topic | `provision` finds a foreign `evbk__log.log_id` | `LogConflict` |
| The database restored from an older backup | `evbk__fence.last_seq` and `last_sequence` regress together with the data | events after the backup are lost; new appends continue from the restored head (ADR-0008 O5) |

## 5. Configuration

```yaml
backends:
  pg-main:
    type: "gts.cf.core.events.backend.v1~cf.core.backend.postgres.v1~"
    url: "postgres://eb:${EB_PG_PASSWORD}@pg-1:5432/events"   # required; ${VAR} / ${VAR:-default} expanded
    schema: "evbk"                     # every object of this plugin; created if absent. Default "evbk"
    pool_max_size: 10                  # the LISTEN connection is one more. Default 10
    pool_acquire_timeout_ms: 5000      # how long an operation waits for a pooled connection. Default 5000
    maintenance_interval_ms: 10000     # the maintenance worker's cadence (§4.9). Default 10000
    segment: { events: 1000000 }       # or { period: "PT1H" }; how the next segment is sized. Default events 1000000
    pgbouncer_transaction_mode: false  # true is refused (§3.1). Default false
```

- Settings are read by hand, not through serde, so an error names the setting and the rule it breaks and never quotes a value (feature 0007 §2.7). An unknown setting is refused.
- `schema` must be a bare identifier (`[a-z_][a-z0-9_]*`) of at most 63 bytes; it is interpolated unquoted.
- `Debug` prints `url` as `<redacted>`.

## 6. Error Mapping

| Situation | `BackendError` |
|---|---|
| Connection refused, I/O, pool closed, pool timeout, `lock_timeout`, serialization failure, startup task not yet complete | `Unavailable` |
| SQLSTATE `28*` (authentication) | `Unavailable`, logged as an authentication failure |
| No segment for a sequence | `Unavailable` |
| No `evbk__log` row | `NotProvisioned` |
| Foreign log id at provisioning | `LogConflict` |
| `Exact(n)` outside `[Earliest, Latest]` | `OffsetOutOfRange` |
| Unknown or invalid setting | `InvalidConfig` |
| Anything else from PostgreSQL | `Internal` |

## 7. Risks / Trade-offs

- **One row lock per partition per append.** Appends to one partition serialize on its `evbk__partition` row: the price of gap-free, in-order visibility. A hot partition is bounded by one commit round trip per append.
- **Catalog size.** Tables = topics x partitions x live segments. Many topics with long retention and small spans make a large catalog; `period` mode or a larger `events` span keeps it in check.
- **Long retention.** A ten-year window at high rates exceeds what one PostgreSQL holds; tiering is §9.
- **Maintenance lag.** A pass that falls behind the head stops appends for that partition until the next segment exists; creating the next segment at half of the current one, and the pass cadence, are the guard.

## 8. Open Questions

- Ingest database restored from backup: a fence key returns with a lower `seq` than `evbk__fence` holds, so new messages are dropped as duplicates (feature 0007 §6).

## 9. Later

- PostgreSQL partitioning by time inside a segment (sub-partitioning by `occurred_at`) if a retention bound needs finer drops.
- Tiering: detach old segments and move them to cheaper storage or an object store; a detached segment is still identified by its name.
- A log-id check on every append if ADR-0008 O1(c) is adopted: step (2) of §4.2 becomes `SELECT 1 FROM evbk__log WHERE topic_uuid = $t AND log_id = $expected`.
