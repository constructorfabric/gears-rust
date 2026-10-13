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
| Key to partition | `murmur3_x86_32(key, seed 0) & 0x7FFF_FFFF % partitions`, fixed for the life of the plugin |
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

CREATE TABLE evbk__partition (                        -- one row per (topic, partition)
    topic_uuid     UUID    NOT NULL REFERENCES evbk__log,
    partition      INTEGER NOT NULL,
    last_sequence  BIGINT  NOT NULL DEFAULT 0,     -- HWM: highest ever assigned; 0 = nothing yet
    stored_events  BIGINT  NOT NULL DEFAULT 0,     -- counted with every insert and delete
    stored_bytes   BIGINT  NOT NULL DEFAULT 0,
    PRIMARY KEY (topic_uuid, partition)
);

CREATE TABLE evbk__fence (                            -- deduplication, one row per (outbox, ingest partition)
    outbox_id         BIGINT   NOT NULL,           -- the outbox group's 64-bit identity (feature 0005 §2.5)
    partition  BIGINT   NOT NULL,           -- OutboxMessage.partition_id
    last_seq          BIGINT   NOT NULL,           -- last applied OutboxMessage.seq
    PRIMARY KEY (outbox_id, partition)
);
```

Native PostgreSQL types: this database belongs to the plugin, so the gear's portable-types rule does not apply. `evbk__fence` holds a handful of rows per outbox group; nothing is indexed per event for deduplication.

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
    sequence_time  TIMESTAMPTZ   NOT NULL,         -- when it was appended
    occurred_at    TIMESTAMPTZ   NOT NULL,         -- resolve(At), retention
    type_id        VARCHAR(1024) NOT NULL,         -- subscription `types` filter
    subject_type   VARCHAR(1024) NOT NULL,         -- subscription `subject_types` filter
    tenant_id      UUID          NOT NULL,         -- tenant scoping
    stored_bytes   INTEGER       NOT NULL,         -- byte retention
    body           BYTEA         NOT NULL,         -- the event as received; opaque to PostgreSQL
    PRIMARY KEY (sequence)
) PARTITION BY RANGE (sequence);

CREATE INDEX idx_evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3__occurred ON evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3 (occurred_at);
-- 4 + 44 + 10 = 58 <= 63 at 4-digit partitions; the per-segment copies PostgreSQL creates are backend-named, outside the ADR's grammar

CREATE TABLE evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3_s0000000000000001
    PARTITION OF evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3 FOR VALUES FROM (1) TO (1000001);
```

- Names are derived from `(topic_uuid, partition, first_sequence)`; nothing stores them.
- Only the fields a read filters or a retention pass measures are columns; everything else of the event lives in `body`.
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

- **Pool.** `PgPoolOptions` with `max_connections = pool_max_size` and `acquire_timeout`; `after_connect` sets `synchronous_commit = on` and `search_path = {schema}`, and `before_acquire` re-asserts `synchronous_commit` on every checkout. Used by `append`, `read`, `resolve`, `query`, `provision` and the maintenance worker.
- **`LISTEN` connection.** A `PgListener` on its own connection outside the pool (`pool_max_size + 1` connections in all), held in `LISTEN evbk__head`. `try_recv` returning `Ok(None)` means sqlx reconnected and notifications may be lost: the watch emits every subscribed partition's current head again (§4.7). An error reconnects with jittered exponential backoff, 1 s to 30 s.
- **Startup task.** `build` validates the settings, starts one task and returns at once; it never fails because the database is unreachable (feature 0007 §2.7). The task, retrying with backoff until it succeeds or `stop` is cancelled:
  1. refuses PgBouncer transaction mode (`InvalidConfig`, logged; `LISTEN` needs a session);
  2. connects the pool, checks `transaction_isolation = read committed`;
  3. `CREATE SCHEMA IF NOT EXISTS {schema}`;
  4. runs the embedded migrations (`sqlx::migrate!`, §3.2);
  5. connects the `LISTEN` connection and starts the maintenance worker (§4.9).
  Until it completes, every operation returns `Unavailable` and readiness reports the entry as not ready.
- **Stop.** Cancelling `stop` ends the startup task, the listener and the maintenance worker; each finishes its current step, and the pool is closed with a 10 s timeout.

### 3.2 Migrations

- The plugin's own tables (`evbk__log`, `evbk__partition`, `evbk__fence`) come from SQL files embedded with `sqlx::migrate!("./src/migrations")`, run by the startup task. sqlx serialises concurrent runs with its own advisory lock and records versions in `_sqlx_migrations`.
- The files use no `IF NOT EXISTS`: a schema that drifted from the recorded version fails the startup task loudly rather than being patched over.
- The plugin keeps its objects in its own schema, `evbk` by default, so its `_sqlx_migrations` never meets another sqlx user's (the cluster plugin numbers its migrations from `0001` too). Table names keep the `evbk__` prefix as well (`docs/arch/database/ADR/0001`).
- Segment tables are not migrations: `provision` and the maintenance worker create them (§4.1, §4.9).

### 3.3 Lock Order

Every transaction that takes several row locks takes them in this order, so two appends never deadlock:

```
1. evbk__fence (outbox_id, partition)
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
    FOR VALUES FROM (1) TO (1 + first_span);
INSERT INTO evbk__partition (topic_uuid, partition) VALUES ($t, p) ON CONFLICT DO NOTHING;
COMMIT;
-> Ready { partitions: evbk__log.partitions }
```

- DDL is transactional in PostgreSQL: a failed provisioning leaves nothing behind.
- `partitions > 9999` is refused with `InvalidConfig` (naming, §2.2).
- `evbk__log.partitions` is the count reported; a different count requested later is the gear's concern (feature 0006 §3.1).

### 4.2 append

`AppendRequest { fence: { outbox, partition, seq }, topic, events, flush }`, here one event whose key maps to partition 3:

```sql
BEGIN;                                                        -- READ COMMITTED
-- (1) fence: first statement; takes the (outbox, partition) row lock
INSERT INTO evbk__fence (outbox_id, partition, last_seq) VALUES ($o, $p, $seq)
ON CONFLICT (outbox_id, partition)
DO UPDATE SET last_seq = EXCLUDED.last_seq
WHERE evbk__fence.last_seq < EXCLUDED.last_seq;
--   0 rows -> ROLLBACK; Ok(Duplicate)

-- (2) provisioned?  (cached per topic; a miss reads evbk__log)
--   no row -> ROLLBACK; NotProvisioned

-- (3) assign sequences: row lock on (topic, 3), held until COMMIT
UPDATE evbk__partition
   SET last_sequence = last_sequence + $n,
       stored_events = stored_events + $n,
       stored_bytes  = stored_bytes + $bytes
 WHERE topic_uuid = $t AND partition = 3
RETURNING last_sequence;                                      -- the batch's last; events take the $n before it

-- (4) the events; PostgreSQL routes each row to its segment
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
| Two outbox groups, or two deployments, writing one database | different `outbox_id`, different rows | independent |

### 4.4 Concurrent Ingest Partitions and Visibility

```
ingest partition 5 (seq 1207): key A -> partition 3      ingest partition 9 (seq 330): key B -> partition 3
T1: fence (o,5) -> evbk__partition (t,3) -> 42
T2: fence (o,9) -> waits on evbk__partition (t,3)
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
-- Latest = HWM
SELECT last_sequence FROM evbk__partition WHERE topic_uuid = $t AND partition = $p;
-- Earliest = oldest stored - 1, or the HWM when nothing is stored
SELECT COALESCE(MIN(sequence) - 1, $hwm) FROM evbk__{t}_p{p};
-- At(t) = (first event with occurred_at >= t) - 1, else Latest
SELECT sequence - 1 FROM evbk__{t}_p{p} WHERE occurred_at >= $t ORDER BY occurred_at, sequence LIMIT 1;
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

The plugin's own worker, started by `build`; the gear never calls into it. Every process that builds the registry entry runs one, so each pass over a `(topic, partition)` first takes `pg_try_advisory_xact_lock(hashtext('evbk:' || topic_uuid || ':' || partition))` and skips the partition if another process holds it. The bounds come from `evbk__log.max_age` / `max_bytes`, written by `provision` (feature 0007 §2.6). Each pass over a `(topic, partition)`:

```
1. ahead:    if the last segment's upper bound <= last_sequence + headroom:
               CREATE TABLE evbk__{t}_p{p}_s{next} PARTITION OF ... FOR VALUES FROM (next) TO (next + span)
2. expire:   oldest_permitted = now() - max_age; for each segment from the oldest, while its newest occurred_at < oldest_permitted:
               lock evbk__partition (t, p); DROP TABLE segment; subtract its counts and bytes; unlock
3. boundary: DELETE FROM evbk__{t}_p{p} WHERE occurred_at < oldest_permitted     -- the partly expired segment
             and, with a byte bound, the oldest rows until stored_bytes <= max_bytes
             counts and bytes adjusted in the same transaction
```

Segment span:

| `segment` setting | Span of the next segment |
|---|---|
| `events: N` (default `1000000`) | exactly `N` sequences |
| `period: P` (ISO 8601 duration) | `rate x P`, `rate` being the partition's observed events per second over the previous segment, with a floor for idle partitions; a segment covers about `P`, more or less under traffic changes |

- `headroom` keeps at least one empty segment ahead of the head; the worker's cadence is short enough to stay ahead of the fastest partition, and a missing segment makes `append` fail with `Unavailable` and the outbox retries.
- The worker runs whether or not the topic has a retention bound: creating space ahead of the head is part of it.
- On `stop` the worker finishes the partition it holds and exits.
- `last_sequence` is never lowered: dropping or deleting events never moves the HWM.
- Retention by time is accurate to one segment; the boundary `DELETE` makes it exact.

### 4.10 Route Changes, Foreign Logs, Lost Data

| Case | What the plugin sees | Result |
|---|---|---|
| A provisioned topic routed here by a routing change | no `evbk__log` row | `NotProvisioned` |
| The database holds a different log id for the topic | `provision` finds a foreign `evbk__log.log_id` | `LogConflict` |
| The database restored from an older backup | `evbk__fence.last_seq` and `last_sequence` regress together with the data | events after the backup are lost; new appends continue from the restored head (ADR-0008 O5) |

## 5. Configuration

```rust
#[derive(Deserialize, toolkit_macros::ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct PostgresBackendSettings {
    /// Connection string; `${VAR}` / `${VAR:-default}` expanded from the environment.
    #[expand_vars]
    pub url: String,
    /// Schema for every object of this plugin, created if absent. Default: "evbk".
    #[serde(default = "default_schema")]
    pub schema: String,
    /// Pool size; the LISTEN connection is one more. Default: 10.
    #[serde(default = "default_pool_size")]
    pub pool_max_size: u32,
    /// How long an operation waits for a pooled connection. Default: 5 s.
    #[serde(default = "default_acquire_timeout")]
    pub pool_acquire_timeout: Duration,
    /// How the next segment is sized. Default: { events: 1000000 }.
    #[serde(default)]
    pub segment: SegmentRule,
}

#[serde(rename_all = "snake_case")]
pub enum SegmentRule {
    /// A fixed number of sequences per segment.
    Events(u64),
    /// About one period of traffic per segment, sized from the observed rate.
    Period(IsoDuration),
}
```

```yaml
backends:
  pg-main:
    type: "gts.cf.core.events.backend.v1~cf.core.backend.postgres.v1~"
    url: "postgres://eb:${EB_PG_PASSWORD}@pg-1:5432/events"
    segment: { events: 1000000 }
```

- `Debug` prints `url` as `<redacted>`.
- `schema` must be a bare identifier of at most 63 bytes; it is interpolated unquoted.
- A settings error names the setting and the rule broken, never a value (feature 0007 §2.7).

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
- **Maintenance lag.** A pass that falls behind the head stops appends for that partition until the next segment exists; `headroom` and the pass cadence are the guard.

## 8. Open Questions

- Ingest database restored from backup: an `outbox_id` returns with a lower `seq` than `evbk__fence` holds, so new messages are dropped as duplicates (feature 0007 §6).
- `period` mode: the floor rate for idle partitions and the lookback for the observed rate.

## 9. Later

- PostgreSQL partitioning by time inside a segment (sub-partitioning by `occurred_at`) if a retention bound needs finer drops.
- Tiering: detach old segments and move them to cheaper storage or an object store; a detached segment is still identified by its name.
- A log-id check on every append if ADR-0008 O1(c) is adopted: step (2) of §4.2 becomes `SELECT 1 FROM evbk__log WHERE topic_uuid = $t AND log_id = $expected`.
