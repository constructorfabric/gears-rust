# Testing - MySQL/MariaDB Event Broker Plugin

Three layers. Layers 2 and 3 need a Docker daemon and run behind the `integration` feature:

```bash
cargo test -p cf-gears-mysql-event-broker-plugin                                                   # layer 1
cargo test -p cf-gears-mysql-event-broker-plugin --features integration -- --test-threads=1        # layers 1-3
GEARS_TEST_MYSQL_TAG=8.0 GEARS_TEST_MARIADB_TAG=10.6 cargo test -p cf-gears-mysql-event-broker-plugin --features integration -- --test-threads=1
```

Every layer 2 and 3 test runs on both engines: the MySQL image is `libs/test-containers`' pinned `mysql` tag (`GEARS_TEST_MYSQL_TAG` overrides it), the MariaDB image its pinned `mariadb` tag (`GEARS_TEST_MARIADB_TAG`). `EB_TEST_MYSQL_URL` / `EB_TEST_MARIADB_URL` point them at an existing server instead (`mysql://user:password@host:3306/any_database`); the two tests that change or restart the server then skip. Each container is named `eb-my-test-<purpose>-<6 hex>` or `eb-maria-test-...` and removed when its test ends.

The matrix the plugin is checked on: MySQL 8.0, 8.4 and the pinned 9.x; MariaDB 10.6, 11.4 and the pinned 11.8.

## 1. Layer 1 - unit tests (no database)

| What | Where |
|---|---|
| Settings: defaults, every setting read, unknown settings refused, bad values refused without quoting them, the `database` identifier rule, the background pool's minimum, `url` expansion and redaction; a url the driver does not accept refused at build; segments of at least one full append; both bounds of `poll_interval_ms` and `rescan_interval_ms` | `src/settings_tests.rs` |
| Dialects: versions of both engines parsed (the `5.5.5-` prefix included), each engine's floor, the isolation variable; the durability shortfalls, `innodb_flush_log_at_trx_commit = 3` durable on MariaDB only, a statement-based binary log refused; Galera and multi-primary Group Replication refused (§3.2, §7.1) | `src/dialect_tests.rs` |
| Error mapping: a refused statement by operation, every transient error `Unavailable` in every operation, access denied, connection limits and commit failures `Unavailable`, a missing table a failure of the operation, driver failures by kind only, no message quoting the server's or the driver's text (§6) | `src/error_tests.rs` |
| Object names: table, segment and lock names, the 64-byte limits at 9999 partitions and a 64-byte database name, the database in the lock name | `src/names_tests.rs` |
| Body encoding: round trip, `meta` dropped, unknown `v` refused | `src/body_tests.rs` |
| Segment span (`events`, `period`, the 1 event/s floor, never below one full append), room and the room alarm, the first segments, a range bound from `PARTITION_DESCRIPTION`, an event's weight | `src/segment_tests.rs` |
| What a head calls for: a layout's room and seal alarms, offers only for unknown and due partitions (§4.9) | `src/maintenance/hub_tests.rs` |

## 2. Layer 2 - conformance (`tests/conformance.rs`)

The shared suite (`plugins/event-broker-backend-conformance`, all scenarios) once per engine: `conformance_mysql` and `conformance_mariadb`, each against one container. `build` names one database, `build_isolated` a database of its own, `build_unreachable` a port nothing listens on. The maintenance rescans every 200 ms and the head poll runs every 100 ms, so the retention and watch scenarios see them act.

## 3. Layer 3 - plugin-specific (`tests/mysql.rs`)

Each test runs as `mysql::<name>` and `mariadb::<name>`, on a server and a database of its own; `make test-eb-mysql` runs every test whose name does not contain `mariadb`, `make test-eb-mariadb` the others. Segments are 1000 events, the smallest allowed. A wait on a lock is checked by bounding it (30 s) while the holder still holds, never by timing it.

| DESIGN | Case | Test |
|---|---|---|
| §3.1 | the dialect, durability, isolation and migrations are checked, and operations are `Unavailable` until startup completes | every test's `common::started`; conformance `start_succeeds_with_the_medium_unreachable` |
| §4.1 | `provision` creates each partition's table with the segments giving room for one append, an `evbk__partition` row each and an `open` row per segment, with the names stored and held by the server | `provision_creates_tables_and_first_segments` |
| §4.1 | a re-provision of a written topic, with an open append's row lock and metadata lock held, finishes at once | `a_re_provision_does_not_wait_for_an_open_append` |
| §4.2 | an append to an unprovisioned topic with a fence that has passed its seq is `NotProvisioned`, not `Duplicate` | `a_replayed_fence_on_an_unprovisioned_topic_is_not_provisioned` |
| §4.3 | a retry blocked behind a hung transaction's fence row fails after the 5 s row lock wait, `Unavailable` (`1205`) | `a_hung_fence_row_times_out_the_retry` |
| §4.3, §4.4 | retries, late copies, gaps, keys apart; two instances appending to one partition get distinct, gapless sequences | conformance `dedup_*`, `concurrent_appenders_assign_distinct_sequences` |
| §4.9 | the maintenance keeps segments ahead: three appends of a thousand events with segments of 1000 all land, gapless, the full segments sealed and two empty segments left, catalog and server agreeing | `segments_are_kept_ahead_of_appends` |
| §2.2, §4.9 | a full segment is sealed with its events' weight (bodies plus 48 bytes each, checked against `SUM(LENGTH(body))`) and its last `seq_time`; the open one is not; `evbk__partition.bytes` is the total | `the_sealer_weighs_full_segments` |
| §4.5, §4.6, §4.9 | by age: sealed segments past `max_age` are expired with the floor and dropped, the open one stays, `Earliest` and `Latest` follow, a read from the start begins past the floor | `retention_by_age_drops_whole_sealed_segments` |
| §4.9 | by bytes: the oldest sealed segments go while what stays holds `max_bytes`, and the next stays; `floor_seq`, `removed_bytes` and the partitions held are exact | `retention_by_bytes_keeps_the_newest_bytes_within_the_bound` |
| §7.2 | a transaction holding the table's metadata lock keeps `ADD PARTITION` from completing; appends behind the retried DDL all succeed within seconds; once the holder ends, the segments are added and every row settled | `a_ddl_waiting_on_a_metadata_lock_does_not_stop_appends` |
| §2.3, §4.9 | every crash between a DDL and its catalog write is settled by the next instance: an `adding` row with its partition opened, one without deleted, an `expired` row with its partition dropped, one without deleted, a partition with no row adopted | `a_crash_between_ddl_and_catalog_is_settled` |
| §7.1 | acknowledged appends survive a restart of the server; numbering continues | `appended_events_survive_a_server_restart` |
| §7.1 | `innodb_flush_log_at_trx_commit = 2` keeps the backend `Unavailable`, its reason naming the variable, and `require_durable_server: false` starts it | `a_server_that_does_not_flush_every_commit_is_refused_unless_allowed` |
| §4.2 | an event after one stored with a clock an hour ahead gets that event's `seq_time`, not an earlier one | `a_clock_step_back_does_not_lower_seq_time` |
| §3.1 | `stop` with its deadline passed, after a full start or during one, leaves no connection to the database open | `stop_closes_every_connection_even_past_its_deadline` |
| §4.7 | a watch yields the current head first, then only named partitions, rising; a waiting read on one instance returns another's append | conformance `watch_*`, `tail_read_sees_another_instance` |
| §6 | `NotProvisioned`, `LogConflict`, `OffsetOutOfRange`, `Unavailable` for an unreachable server | conformance |

## 4. Bench (`benches/backend.rs`)

The shared bench (`plugins/event-broker-backend-bench`) against a container on the pinned `mysql` tag, or MariaDB with `EB_BENCH_MYSQL_ENGINE=mariadb`, or the server `EB_BENCH_MYSQL_URL` names, with the plugin's default settings (only `url`, `database` and the main pool's size are set):

```bash
cargo bench -p cf-gears-mysql-event-broker-plugin --features integration --bench backend -- --quick --scenarios b1,b2
```

Not yet covered:

- startup with the server down at first and up later (`Unavailable`, then served without a restart);
- a server below either floor being refused (no older image is pulled);
- two instances' maintenance contending for one partition's named lock: covered only through `provision` waiting on it;
- `period` segment sizing from an observed rate end to end;
- the 8192-partition limit of one table;
- Galera and multi-primary Group Replication being refused end to end (layer 1 covers the decision; no such cluster is started).
