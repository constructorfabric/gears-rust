# Technical Design - MySQL/MariaDB Event Broker Plugin

- [1. Overview](#1-overview)
  - [1.1 Role](#11-role)
  - [1.2 Contract Coverage](#12-contract-coverage)
- [2. Domain Model](#2-domain-model)
  - [2.1 Plugin Tables](#21-plugin-tables)
  - [2.2 Event Tables and Segments](#22-event-tables-and-segments)
  - [2.3 Segment States](#23-segment-states)
- [3. Component Model](#3-component-model)
  - [3.1 Connections and Startup](#31-connections-and-startup)
  - [3.2 Dialects](#32-dialects)
  - [3.3 Migrations](#33-migrations)
  - [3.4 Locks](#34-locks)
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
- [5. Configuration](#5-configuration)
- [6. Error Mapping](#6-error-mapping)
- [7. Server Prerequisites and Operations](#7-server-prerequisites-and-operations)
  - [7.1 Durability](#71-durability)
  - [7.2 Metadata Locks](#72-metadata-locks)
  - [7.3 Files and Partitions](#73-files-and-partitions)
- [8. Risks / Trade-offs](#8-risks--trade-offs)
- [9. Later](#9-later)

## 1. Overview

### 1.1 Role

A storage backend for the event-broker gear over MySQL 8.0+ or MariaDB 10.6+. It implements `backend::Backend` and ships a `backend::Provider` for the backend type `gts.cf.core.events.backend.v1~cf.core.backend.mysql.v1~` (event-broker feature 0007), the same type for both engines: the engine is detected at start (§3.2). One `backends` registry entry (feature 0005 §2.1) is one database this plugin owns: the gear's own database is never used.

References:

- event-broker `docs/features/0005-backend-registry-and-routing.md` - registry entries, outbox groups and their identity
- event-broker `docs/features/0006-topic-provisioning.md` - provisioning attempts, log id
- event-broker `docs/features/0007-storage-backend-api.md` - the trait this plugin implements
- event-broker `docs/ADR/0001-offset-semantics.md` - sequences from 1, cursor 0

### 1.2 Contract Coverage

| Contract item | This plugin |
|---|---|
| `append` result | `Heads` |
| `dedup()` | `Fenced`: the fence row and the events commit in one transaction |
| `watch()` | `Native`: the instance's own heads, fed by its appends and by one batched poll of the watched partitions (§4.7) |
| `provision` | `Ready` once every partition's table exists; `Pending` while another session holds a partition's lock (§4.1) |
| `max_events_per_append()` | 1000 |
| Key to partition | `(murmur3_x86_32(key, 0) & 0x7FFF_FFFF) % partitions` (`event_broker_sdk::backend::support::partition_for`), fixed for the life of the plugin |
| Sequences | per partition, `BIGINT` from 1, gapless, assigned under a row lock; visible in sequence order |
| `Flush` | `Sync` and `Async` behave the same: every append commits before it returns |
| Retention | whole segments, by `seq_time` (when the event was stored), never by `occurred_at` (§4.9) |

## 2. Domain Model

### 2.1 Plugin Tables

Created by the plugin's migration (`src/migrations/0001_plugin_tables.sql`, which lists every query that touches each table). Every table is `InnoDB`, `utf8mb4_bin`, with no foreign keys: `InnoDB` would take shared locks on the parent rows that appends lock.

| Table | One row per | Holds |
|---|---|---|
| `evbk__log` | topic | `topic_uuid BINARY(16)` (`GtsId::to_uuid`), `topic_id VARCHAR(1024)`, `log_id BINARY(16)` (feature 0006), `partitions`, `max_age_ms`, `max_bytes` (NULL = unbounded), `provisioned_at DATETIME(6)` |
| `evbk__partition` | (topic, partition) | `table_name VARCHAR(64)` as created (authoritative), `hwm` (highest sequence ever assigned), `bytes` (weight of every event ever appended); append's row |
| `evbk__retention` | (topic, partition) | `floor_seq` (highest removed sequence: stored = `(floor_seq, hwm]`), `removed_bytes` (weight of the expired segments); retention's row, apart from append's so the two never wait on each other |
| `evbk__segment` | segment | `[first_seq, end_seq)`, `segment_name` (the range partition, as created), `state` (§2.3), `bytes` and `newest_at` once sealed, `changed_at` |
| `evbk__fence` | fence key | `fence_key BINARY(16)` (the key's 16 bytes, opaque), `last_seq` |

`partition` is a reserved word on both engines, so the column is `partition_no`. Every `DATETIME(6)` is UTC: each connection runs with `time_zone = '+00:00'`, and storage times come from `UTC_TIMESTAMP(6)`.

### 2.2 Event Tables and Segments

Each (topic, partition) has one table, `evbk__{topic_uuid}_p{partition}` (44 bytes at most), `PARTITION BY RANGE (seq)`:

| Column | Type | |
|---|---|---|
| `seq` | `BIGINT` | primary key, the partitioning column |
| `seq_time` | `DATETIME(6)` | when the append stored it; never below the previous event's, so it rises with `seq` even across a server clock stepped back (§4.2) |
| `tenant_id` | `BINARY(16)` | |
| `cum_bytes` | `BIGINT` | the partition's running weight through this event |
| `body` | `LONGBLOB` | everything else, as versioned JSON (`src/body.rs`); `meta` and the positions are not stored |

Each range partition is a *segment*, named `s{first_seq:016x}`, holding `VALUES LESS THAN (end_seq)`; its lower bound is the previous one's upper bound. A segment spans `segment.events` sequences, or under `segment.period` the observed rate of the two newest sealed segments times the period, at least one event per second. Either way a segment spans at least 1000 sequences, one full append: a smaller span would reach a table's 8192-partition limit within a few appends (§7.3). Names are derived once, when a table or segment is created, and stored with its row; the stored name is the one used afterwards.

An event's *weight* is its body's length plus a fixed 48-byte allowance for its other columns and row header. `append` adds the weight of what it stores to `evbk__partition.bytes` and writes the running total into each row's `cum_bytes`, so a full segment's weight is two numbers already on disk (§4.9) and no statistic of the server is consulted: `information_schema` table sizes are cached and approximate.

### 2.3 Segment States

DDL commits on its own on both engines, so no DDL shares a transaction with the catalog. Each one is bracketed by catalog writes instead, and every state a crash can leave is one a later maintenance run settles from `information_schema.PARTITIONS` (§4.9):

| State | Meaning | Written |
|---|---|---|
| `adding` | the range partition is being added, or the table created; the server may or may not hold it | before `ADD PARTITION` / `CREATE TABLE` |
| `open` | the range partition exists and is not full | after the DDL |
| `sealed` | full, weighed (`bytes`) and timed (`newest_at`) | by the sealer |
| `expired` | at or below the floor; dropped next, then the row deleted | with the floor, in one transaction |

## 3. Component Model

### 3.1 Connections and Startup

`build` validates the settings and connects nothing. `start` spawns the startup task and returns at once; until it completes, every operation is `Unavailable`. The task retries with backoff (1 s doubling to 30 s, jittered) until it succeeds or the backend is stopped:

1. connect with the url as given (10 s at most per attempt), read `SELECT VERSION()`, detect the dialect and refuse a release below its floor (§3.2);
2. refuse a server that more than one node writes to: Galera (`wsrep_on = ON`) or multi-primary Group Replication (`group_replication_single_primary_mode = OFF`), read with `SHOW GLOBAL VARIABLES`, which answers no row on a server without them (§7.1);
3. read the durability variables and refuse, or warn of, a server that does not make every commit durable (§7.1);
4. `CREATE DATABASE IF NOT EXISTS {database}`;
5. open the main pool in that database, every connection set to `READ COMMITTED` and `innodb_lock_wait_timeout = 5`, and check the isolation took;
6. run the migrations;
7. start the head poll (§4.7) and, in an ingest or standalone process, open the background pool lazily and start the maintenance (§4.9).

`READ COMMITTED` takes no gap locks, and every statement sees what committed before it began. The 5 s row lock wait bounds how long a retry waits behind a hung append's fence row.

A delivery process holds the main pool only, and takes no named lock. `stop` marks the backend stopped, ends every task, waits for them until its deadline, and closes both pools - on a task of its own once the deadline has passed, so the pools close either way. A startup attempt still running is dropped where it stands, and one that completes after `stop` closes its pools instead of installing them. Every operation after `stop` is `Unavailable`.

The url is parsed when the entry is built: one that is not a `mysql://` (or `mariadb://`) url the driver accepts is `InvalidConfig`, not a backend that retries for good.

### 3.2 Dialects

Every statement the plugin sends is the same on both engines. What differs is confined to `src/dialect.rs`:

| | MySQL | MariaDB |
|---|---|---|
| Detected from `VERSION()` | anything without `MariaDB`: `8.0.39`, `8.4.3-log` | contains `MariaDB`: `11.8.2-MariaDB-ubu2404`; a `5.5.5-` prefix is skipped |
| Oldest supported | 8.0.0: atomic DDL, in-place `ADD`/`DROP PARTITION` on InnoDB | 10.6.0: atomic DDL arrived in 10.6 |
| Session isolation variable | `transaction_isolation` | `tx_isolation` (the only name before 11.1) |
| Default binary log | on, `ROW`, `sync_binlog = 1` | off |
| Durable `innodb_flush_log_at_trx_commit` | `1` | `1`, or `3` (flushed at prepare and at commit: as durable as `1`, per the MariaDB documentation) |
| Multi-writer replication refused | multi-primary Group Replication | Galera (`wsrep_on`) |

Atomic DDL matters to §2.3: a single `CREATE TABLE`, `ADD PARTITION` or `DROP PARTITION` either happened or did not, so the only states to settle are between a DDL and its catalog write, never inside a DDL.

### 3.3 Migrations

`sqlx::migrate!` over `src/migrations`, recorded in `_sqlx_migrations` in the plugin's database, under sqlx's own `GET_LOCK`, so instances starting together migrate once. The migration has no `IF NOT EXISTS`: a database that drifted from the recorded version keeps the backend `Unavailable` and says so in the log.

### 3.4 Locks

| Lock | Taken by | Wait |
|---|---|---|
| `evbk__fence` row, `FOR UPDATE` | `append`, first | 5 s (`innodb_lock_wait_timeout`) |
| `evbk__partition` row | `append`, per partition, ascending | 5 s |
| `evbk__log` row, `FOR UPDATE` | `provision` | 5 s |
| named lock `evbk.{database hash}.{topic_uuid}.{partition}` (`GET_LOCK`, at most 59 bytes) | a maintenance run (none), `provision` creating a table (5 s, then `Pending`) | per caller |
| metadata lock of an event table, exclusive | `CREATE TABLE`, `ADD PARTITION`, `DROP PARTITION` | `maintenance.ddl_lock_wait_s`, longer per retry (§7.2) |

Named locks are server-wide, so the database is part of the name, hashed (FNV-1a 64) to fit the 64-byte limit. A named lock belongs to its session: a maintenance run releases it before its connection goes back to the pool, and a run cut off by an error or a stop closes its connection instead, which ends the session and the lock with it.

A statement or transaction chosen as a deadlock victim (`1213`) - by `InnoDB`, or by the metadata lock subsystem when a DDL closes a cycle, which can fail even a plain read - is run again, up to three runs in all, before the error is returned.

## 4. Operations

### 4.1 provision

1. Refuse a partition count outside `1..=9999` (`InvalidConfig`): four digits keep every name within 64 bytes.
2. In one transaction: `INSERT IGNORE` the `evbk__log` row, read it back `FOR UPDATE`, refuse a stored log id the claim does not accept (`LogConflict`), set the claim's id and the spec's retention (an age above 100 years is stored as 100 years), and insert the `evbk__partition` and `evbk__retention` rows of each partition that has none. A partition keeps the table name it was given first.
3. For each partition whose table the server does not hold: a maintenance run (§4.9) under the partition's named lock, waiting up to 5 s for it, which records the first segments `adding`, creates the table with them, and opens them. A busy lock answers `Pending`.
4. `Ready` with the partition count the log row holds.

A re-provision of a written topic finds every table and takes no lock and runs no DDL, so it never waits for an open append. The first segments give room for one full append: as many as needed for two to lie wholly above the head and 1000 sequences past it.

### 4.2 append

1. Refuse more than 1000 events (`AppendFailed`). Look the topic up (cached for the life of the process): no log row is `NotProvisioned`, checked before the fence, so a fence that has passed this seq is no answer for a topic the backend does not hold.
2. Group the events by partition and encode them, before any transaction opens.
3. In one transaction:
   1. the fence: `SELECT last_seq ... FOR UPDATE`; at or above the seq is `Duplicate` and nothing is written; below it, `UPDATE`; no row, `INSERT`, and a duplicate-key error from a concurrent first applier reads the row again;
   2. per partition, ascending: `UPDATE evbk__partition SET hwm = hwm + n, bytes = bytes + w`, then, under the row lock just taken, the new head, the running weight, the server's `UTC_TIMESTAMP(6)` and the previous event's `seq_time` (by its key, `hwm - n`); the storage time is the later of the two, so it never goes back within a partition, even when the server's clock does;
   3. multi-row `INSERT`s of the events, at most 4 MiB of bodies each (the smallest default `max_allowed_packet` is MariaDB's 16 MiB).
4. Commit; publish the heads to the instance's watchers and waiting reads, and offer each partition whose head crossed an alarm to the maintenance.

`UPDATE ... RETURNING` exists on neither engine, hence the read after the update; and the driver reports matched rather than changed rows, so the fence is read and compared rather than told apart by an affected-row count.

An insert that finds no segment for a sequence (`1526`) means the maintenance fell behind: the partition is offered at once, and the append is `Unavailable`, for the outbox to retry.

### 4.3 Deduplication Cases

| Case | Outcome |
|---|---|
| A retry of an append that committed | the fence row holds its seq: `Duplicate` |
| A retry while the first attempt is still open | waits on the fence row; then `Duplicate` if it committed, or applies if it rolled back |
| A retry behind a hung transaction | fails after 5 s, `Unavailable` (`1205`) |
| A late copy after a later fence of the same key | `Duplicate` |
| A seq above the last, with gaps | applied |
| The first append of a key, raced by another | one inserts; the other reads the committed row and compares |

### 4.4 Concurrent Ingest Partitions and Visibility

Appends to one partition serialize on its `evbk__partition` row: the head and the rows it covers commit together, so a reader that sees a head sees every sequence below it, and sequences have no gaps. Appends to different partitions run in parallel; a multi-partition append locks its partitions in ascending order, so two never deadlock on each other.

### 4.5 read

`SELECT seq, seq_time, body FROM {table} WHERE seq > GREATEST(?, (SELECT floor_seq FROM evbk__retention ...)) ORDER BY seq LIMIT ?`: one statement, so the floor and the rows are read in one snapshot and no `DROP PARTITION` runs between them. Expired segments stay until dropped; the floor hides them from the commit that expired them.

A waiting read (`Wait::For`) reads, then waits for a head above its offset from this instance's appends or head poll, re-reading at least every `poll_interval`, until it has events or the wait is over.

### 4.6 resolve

`Latest` is `hwm`, `Earliest` is `floor_seq`, both from one statement. `Exact(n)` outside `[floor, hwm]` is `OffsetOutOfRange`. `At(t)` is one below the first event stored at or after `t`, or `hwm` when there is none: `seq_time` rises with `seq`, so every event of a sealed segment is no later than its `newest_at`, and the primary-key walk starts past the last segment sealed before `t` - one segment at most is scanned, and no index on `seq_time` is kept.

### 4.7 watch

`Native`, over the instance's own heads (`support::LocalHeads`). A watch first yields the current head of every named partition (one batched query), then every head published above it: by this instance's appends at once, and by the instance's head poll, which reads `hwm` of every partition any open watch names in one query per 500 partitions every `poll_interval` (default 500 ms). A watch counts its partitions in while it lives; dropping it counts them out. The servers push nothing, so a head appended through another instance reaches a watch within one `poll_interval`.

### 4.8 query

The matched range's first and last sequence and storage time from the event table, above the floor, and one entry per live (`open` or `sealed`) segment from the catalog: `{ name, from_sequence, to_sequence }`.

### 4.9 Maintenance

Runs in ingest and standalone processes only, over the background pool: one worker and one rescan per instance.

**The worker** takes the partitions offered to it and runs every step for one at a time, under the partition's named lock taken without waiting (held by another session: that session does the work, and the next rescan offers it again):

1. **settle** - each `adding` row whose range partition the server holds is opened, and deleted when it does not; each `expired` row whose range partition is gone is deleted; a range partition with no row is adopted as `open` above the floor, and dropped at or below it; an `open` or `sealed` row whose range partition is gone is logged as lost events and left;
2. **extend** - the table is created when the server holds none (and no live row says it should), else range partitions are added in one `ADD PARTITION` until two lie wholly above the head and 1000 sequences past it; rows `adding` before the DDL, `open` after;
3. **seal** - each open segment whose last sequence is at or below the head is full: an append commits its rows with the head and sequences have no gaps. Its weight is the running weight of its last stored row minus everything below it (`removed_bytes` and the sealed segments' weights); `newest_at` is that row's `seq_time`;
4. **retain** - on a topic with a bound, the oldest sealed segments go while `aged` (its `newest_at` is older than `max_age` before the server's now, or it has no event left) or `over` (what stays without it still weighs at least `max_bytes`), oldest first, stopping at the first that is neither; `floor_seq`, `removed_bytes` and the rows' `expired` state commit in one transaction;
5. **drop** - the expired range partitions in one `DROP PARTITION`, then their rows. A table keeps at least one range partition.

Each run sets the session's `lock_wait_timeout` to `ddl_lock_wait_s` times the attempt (up to four times) and back to the server's default after it. A failed run is retried after 0.5 s, doubling up to 30 s, and reported at `error` from its fifth failure in a row; a run that finds the database unreachable backs the whole worker off instead.

**Offers.** `append` checks each new head against what this instance knows of its partition: the head at which room gets short, and the head at which its oldest open segment is full. Crossing either offers the partition; an unknown partition is always offered.

**The rescan**, every `rescan_interval`, reads every partition and segment row, replaces what the instance knows, and offers every partition that needs room or a seal, whose oldest sealed segment is past a bound, or that holds an `adding` or `expired` row. Every `crosscheck_every` passes, the first included, it offers every partition, so each one's rows are compared with the server's partitions.

## 5. Configuration

```yaml
backends:
  mysql-main:
    type: "gts.cf.core.events.backend.v1~cf.core.backend.mysql.v1~"
    url: "mysql://eb:${EB_MYSQL_PASSWORD}@mysql-1:3306/mysql"   # required; ${VAR} expanded; the server, any database
    database: "evbk"                   # every object of this plugin; created if absent. Default "evbk"
    pools:
      main: 10                         # every operation and the head poll. Default 10
      background: 2                    # the maintenance worker and the rescan, where they run. Default and minimum 2
    pool_acquire_timeout_ms: 5000      # how long an operation waits for a pooled connection. Default 5000
    segment: { events: 1000000 }       # or { period: "PT1H" }; how the next segment is sized, never below 1000
                                       # events. Default events 1000000
    poll_interval_ms: 500              # the head poll, and a waiting read's re-read; 10-60000. Default 500
    require_durable_server: true       # false: a server that does not flush every commit only warns (§7.1). Default true
    maintenance:                       # every field optional
      rescan_interval_ms: 60000        # between two rescans; 100-86400000. Default 60000
      crosscheck_every: 10             # rescans per pass that offers every partition. Default 10
      ddl_lock_wait_s: 1               # lock_wait_timeout of a DDL's first attempt, 1-60. Default 1
```

- Settings are read by hand, not through serde, so an error names the setting and the rule it breaks and never quotes a value (feature 0007 §2.7). An unknown setting is refused.
- `url` must be a `mysql://` or `mariadb://` url the driver parses; `segment.events` at least 1000; `poll_interval_ms` from 10 to 60000; `maintenance.rescan_interval_ms` from 100 to 86400000.
- `database` must be a bare identifier (`[a-z_][a-z0-9_]*`) of at most 64 bytes; it is interpolated unquoted.
- `Debug` prints `url` as `<redacted>`.
- The settings are the same in every process (feature 0005); the role decides which apply: a delivery process reads neither `pools.background` nor `maintenance`.

## 6. Error Mapping

| Situation | `BackendError` |
|---|---|
| Connection refused, I/O, TLS, protocol, pool closed or timed out, SQLSTATE class `08`; server errors `1205` (lock wait timeout), `1213` (deadlock, after three runs), `1040` (too many connections), `1053` (shutdown), `1317` (interrupted), `1927` (connection killed), `3024` / `1969` (statement timeout), `1290` / `1836` / `1792` (read-only: a failover), `3032` (offline mode), `4031` (idle disconnect), `1203` / `1226` (the account's connection or resource limit), `1180` (error during commit), `3100` / `3101` (a replication hook or certification refused the commit), `1526` (no segment for a sequence); startup not yet complete | `Unavailable` |
| `1044`, `1045`, `1698`, `1227`, `1142` (access denied) | `Unavailable`, logged as access denied |
| No `evbk__log` row | `NotProvisioned` |
| Foreign log id at provisioning | `LogConflict` |
| `Exact(n)` outside `[Earliest, Latest]` | `OffsetOutOfRange` |
| Unknown or invalid setting, or a partition count outside `1..=9999` | `InvalidConfig` |
| Any other server error in `append`, `1146` (missing table: a table dropped by hand, or one a concurrent provisioning has not created yet) included | `AppendFailed` |
| Any other server error in `read`, `resolve`, `watch` or `query` | `ReadFailed` |
| Any other server error in `provision` | `Internal` |
| A driver failure (decode, encode, missing row or column, configuration) | `Internal` |

A message names the operation and the server's error number and SQLSTATE, or the kind of driver failure, and nothing else: the server's message and the driver's own text may quote stored values.

## 7. Server Prerequisites and Operations

### 7.1 Durability

An append returns only once its commit is durable, which the server decides. At start the plugin reads:

| Variable | Needed | Why |
|---|---|---|
| `innodb_flush_log_at_trx_commit` | `1`, or `3` on MariaDB | the redo log is flushed at every commit |
| `sync_binlog` | `1` when `log_bin` is on | the binary log is flushed at every commit, so a crash does not leave the redo log and the binary log disagreeing |
| `binlog_format` | `ROW` or `MIXED` when `log_bin` is on | a statement-based binary log refuses `READ COMMITTED` writes; startup fails |

With `require_durable_server: true` (the default) a server short of the first two keeps the backend `Unavailable`, the error's reason and the log naming what is short; with `false` it only warns. `innodb_doublewrite` is assumed on (the default on both engines) and not checked.

**Single primary only.** The maintenance keeps instances apart with named locks (`GET_LOCK`), which are local to one server. With more than one node taking writes, instances on two nodes could change one partition's segments at once, so startup refuses Galera (`wsrep_on = ON`) and multi-primary Group Replication (`group_replication_single_primary_mode = OFF`); the backend stays `Unavailable` and the log names the topology. A single primary with replicas is supported. The durability check covers the one server the url names: a failover to an asynchronous replica can still lose acknowledged appends, so a replicated deployment that must not lose them uses semi-synchronous replication or single-primary Group Replication.

### 7.2 Metadata Locks

`CREATE TABLE`, `ADD PARTITION` and `DROP PARTITION` need the table's exclusive metadata lock, which waits for every open transaction that has touched the table - an append in flight, a read, an operator's open session - and, while it waits, makes every new statement on that table wait behind it. Hence the short `lock_wait_timeout` of a maintenance run (`ddl_lock_wait_s`, 1 s by default): a DDL behind a long transaction gives up quickly and is retried with backoff and a longer wait, and appends and reads behind it wait seconds at most. Room is made well ahead of the head (a full append's worth beyond two whole segments), so a DDL that keeps failing costs nothing until that room is used up; then appends fail with `Unavailable` (`1526`) until it succeeds.

Operators should keep sessions on the plugin's database short: an idle-in-transaction session that has read an event table blocks its DDL indefinitely. `SELECT * FROM performance_schema.metadata_locks` (MySQL) or `information_schema.METADATA_LOCK_INFO` (MariaDB, plugin `metadata_lock_info`) shows the holder.

### 7.3 Files and Partitions

Every segment is a range partition, and with `innodb_file_per_table` (the default on both engines) every range partition is a tablespace file of its own. Live files = topics x partitions x live segments. Size the server for it:

- `open_files_limit` and `innodb_open_files`: InnoDB keeps a file handle per open tablespace; below the live file count it closes and reopens files under load;
- `table_open_cache` and `table_definition_cache`: one entry per event table (a partitioned table counts once);
- at most 8192 partitions per table on both engines: a partition holding more live segments than that fails `ADD PARTITION`, and its appends stop once its room is used. A larger `segment.events` or `segment.period` keeps the count low; the default of a million events per segment keeps an unbounded partition below the limit up to eight billion events.

Each segment costs a file and a data dictionary entry whatever it holds, so spans near the 1000-event minimum suit tests and low-volume topics only.

## 8. Risks / Trade-offs

- **One row lock per partition per append.** Appends to one partition serialize on its `evbk__partition` row: the price of gapless, in-order visibility. A hot partition is bounded by one commit round trip per append.
- **Whole segments only.** Retention removes sealed segments, never the one being written to: an idle partition keeps up to one open segment of events past its bounds, and a partition bound by bytes keeps between `max_bytes` and `max_bytes` plus its oldest sealed segment.
- **Low-volume topics overrun `max_age` by a segment's fill time.** An event goes no sooner than its segment is full and the segment's newest event is past `max_age`, so it outlives its bound by about the time its partition takes to fill one segment. With the default 1 000 000-event segments that is 11.6 days at 1 event/s per partition and 116 days at 0.1 event/s; a partition that stops receiving events keeps its open segment for good. Under `segment.period` a segment spans at least the period at 1 event/s (and at least 1000 events), so it fills in about one period at 1 event/s or more, and in period x (1 / rate) below it - a day at 0.04 event/s with `PT1H`. Size `segment` from the partition's slowest expected rate where `max_age` must hold closely.
- **`max_bytes` is a logical weight.** Body length plus 48 bytes per event, not what InnoDB takes on disk (pages, indexes, the empty segments ahead, fragmentation): a bound is approximate, counted the same way everywhere and never read from the server's statistics.
- **Heads from other instances are polled.** A watch sees another instance's appends within one `poll_interval`; each instance with watches sends one query per interval per 500 watched partitions.
- **Reads touch every live segment.** The floor comes from a subquery, which the server does not use to prune range partitions, so a read seeks the primary key of every live segment of the partition: cheap while live segments are few, as the default span keeps them.
- **Room is made ahead, not on demand.** An append that outruns the maintenance fails with `Unavailable` and is retried by the outbox; the failed insert offers the partition at once.
- **One maintenance worker per instance.** Partitions are maintained one at a time; with many instances, each partition's named lock keeps one at work on it and the others move on.
- **Lost events are reported, not repaired.** An `open` or `sealed` row whose range partition is gone (dropped by hand) is logged at every run that sees it; the plugin does not create the table again over the loss.
- **No metrics.** The plugin exports none: stuck partitions are visible in the logs only.

## 9. Later

- Per-segment figures (`bytes`, `newest_at`) in `query`'s segment entries.
- Pruning reads by a constant range: read the floor first and bound `seq` from both sides, if the per-segment seek shows up at high segment counts.
- A gauge of stuck partitions, once the plugin exports metrics.
- TLS settings beyond what the url carries.
