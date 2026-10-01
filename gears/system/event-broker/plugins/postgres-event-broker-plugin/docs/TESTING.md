# Testing - Postgres Event Broker Plugin

Three layers. Layers 2 and 3 need a Docker daemon and run behind the `integration` feature:

```bash
cargo test -p cf-gears-postgres-event-broker-plugin                          # layer 1
cargo test -p cf-gears-postgres-event-broker-plugin --features integration   # layers 1-3 and the invariants
make test-eb-pg                                                              # the same, via nextest
```

The PostgreSQL image is `libs/test-containers`' pinned tag (`GEARS_TEST_PG_TAG` overrides it). `EB_TEST_POSTGRES_URL` points layers 2 and 3 at an existing server instead of a container.

## 1. Layer 1 - unit tests (no database)

| What | Where |
|---|---|
| Settings: defaults, every setting read, unknown setting refused, bad values refused without quoting them, `schema` identifier rule, `pgbouncer_transaction_mode: true` refused, `url` expansion and redaction; the `maintenance` object; `pools`, and the background pool holding every pooled worker | `src/settings_tests.rs` |
| Object names: parent, segment and index names, the 63-byte limit at 9999 partitions; a segment name placed back by the cross-check | `src/names_tests.rs` |
| Body encoding: round trip, `meta` dropped, unknown `v` refused | `src/body_tests.rs` |
| Segment span: `events`, `period` from the observed rate, the 1 event/s floor; the first segments `provision` plans | `src/segment_tests.rs` |
| Partition bound parsing from `pg_get_expr(relpartbound)`; the room alarm | `src/segment_tests.rs` |
| Maintenance queues: no lost offer during a lease, priority and offer order, every way a lease settles, postponement delays and doubling, warn and stuck thresholds, `peek`/`take` timing, the wakeup handed on (§4.9) | `src/maintenance/prioritizer_tests.rs` |
| Error classes: every SQLSTATE and error kind, a stale catalog retried once then a bug, the escalating lock and statement timeouts (§4.9) | `src/maintenance/classify_tests.rs` |
| Registry: layouts, both alarms, urgency order, replacing and forgetting (§4.9) | `src/maintenance/registry_tests.rs` |
| Cross-check: each case C1-C10, and partitions or names it leaves alone (§4.9) | `src/maintenance/crosscheck_tests.rs` |
| A job worker over a database that is down: idles on an empty queue, backs off and keeps the partition uncounted, puts the partition back when stopping | `src/maintenance/extender_tests.rs` |

## 2. Layer 2 - conformance (`tests/conformance.rs`)

The shared suite (`plugins/event-broker-backend-conformance`, all scenarios) against one container. `build` names the container's database and one schema; `build_isolated` a schema of its own; `build_unreachable` a port nothing listens on.

## 3. Layer 3 - plugin-specific (`tests/postgres.rs`)

Each test starts its own server (or uses `EB_TEST_POSTGRES_URL`) and a schema of its own.

| DESIGN | Case | Test |
|---|---|---|
| §3.1 | the default isolation, server version and schema are checked, and operations are `Unavailable` until startup completes | every test's `common::started`; conformance `start_succeeds_with_the_medium_unreachable` |
| §3.2 | migrations are recorded in the plugin's own schema; a recorded migration that no longer matches keeps the backend `Unavailable` | `a_drifted_schema_fails_startup` |
| §4.1 | `provision` creates one parent and its first segments per partition, a counter row each and an `open` segment row per segment, with the names stored | `provision_creates_parents_and_first_segments` |
| §4.1 | more than 9999 partitions is refused | `too_many_partitions_is_refused` |
| §4.2 | one row lock per partition: two instances appending to one partition get distinct, gap-free sequences | conformance `concurrent_appenders_assign_distinct_sequences` |
| §4.3 | a late copy after a later fence stores nothing; a retry is `Duplicate` | conformance `dedup_*` |
| §4.3 | a retry blocked on a hung transaction's fence row fails on `lock_timeout` (5 s) instead of waiting | `a_hung_fence_row_times_out_the_retry` |
| §4.7 | after the `LISTEN` connection is killed, a head committed meanwhile reaches a watch | `listener_reconnect_re_emits_heads` |
| §4.9 | nothing is removed while events are within both bounds | conformance `retention_*` |
| §4.9 | the extender keeps segments ahead: two full appends with segments of 500 never fail and end with the expected rows and attached tables | `segments_are_kept_ahead_of_appends` |
| §4.9 | an append that finds no segment is `Unavailable`, offers the extender, and succeeds on retry | `an_append_with_no_segment_recovers` |
| §4.9 | the counter's `bytes` and `newest_at` match a full scan after appends in several steps; a segment seals exactly when full | `the_counter_weighs_segments_and_seals_full_ones` |
| §4.9 | by age: sealed segments expire whole, the open one stays, `floor` and `removed_bytes` are exact, `Earliest` and `Latest` follow; the detacher drops the expired tables and rows | `retention_by_age_expires_whole_sealed_segments` |
| §4.9 | by bytes: never under the bound; while another session holds the retention row nothing moves, and retention resumes once it is released | `retention_by_bytes_never_goes_under_the_bound_and_yields_to_a_holder` |
| §4.5, §4.9 | an expired segment still attached is not read: a read from the start begins past the floor | `retention_by_bytes_never_goes_under_the_bound_and_yields_to_a_holder` |
| §4.9 | a detach left pending is finished with `FINALIZE`; the table dropped, the row deleted | `a_pending_detach_is_finalized` |
| §3.3, §4.9 | a detach waiting on a long reader is cut off by its timeout and left pending, then finished; no advisory lock is left behind | `a_detach_cut_off_by_its_timeout_finishes_and_releases_its_lock` |
| §4.9 | a `detaching` row whose table is already gone is deleted | `a_detaching_row_with_no_table_is_deleted` |
| §3.1, §4.9 | a delivery process holds a main pool and a `LISTEN` connection and runs no maintenance; an ingest process adds the background pool, which closes when it stops | `only_an_ingest_process_runs_the_maintenance` |
| §4.9 | an attached table with no row in the extender's way is adopted as soon as the extender hits it, with the cross-check out of the way | `a_name_collision_is_adopted_at_once` |
| §4.9 | the cross-check repairs C4 (row deleted, both made again), C7 (table adopted) and C8 (table dropped), and only reports C5 | `the_cross_check_repairs_drift_and_reports_lost_events` |
| §6 | `NotProvisioned`, `LogConflict`, `OffsetOutOfRange`, `Unavailable` for an unreachable server | conformance |

## 4. Storage invariants (`tests/sql/invariants.sql`, `tests/invariants.rs`)

One definition of intact storage, run from the layer 3 tests, the long-haul run and `psql` alike: `pg_temp.evbk_violations(schema, mode)` returns one row per violation, and creates nothing in the plugin's schema. The caller reads in one `REPEATABLE READ` snapshot; the catalog is not in it, so a live finding counts only once two checks in a row report it.

| Id | Holds | Mode |
|---|---|---|
| L1 | `0 <= floor <= hwm` | live |
| L2 | exactly `hwm - floor` events in `(floor, hwm]`, none above `hwm` | live |
| L3 | `appended_bytes - removed_bytes` = `SUM(stored_bytes)` over `(floor, hwm]` | live |
| L4 | open and sealed rows start at `floor + 1` and tile without gap or overlap | live |
| L5 | expired and detaching rows end at or below the floor; open and sealed above it | live |
| L6 | a sealed row is counted to its end, and its `bytes` match a scan | live |
| L7 | an open row's `counted_to` lies in `[first_seq - 1, max(first_seq - 1, min(hwm, end_seq - 1))]`, its `bytes` match a scan, `newest_at` is null exactly when nothing is counted | live |
| L8 | every open, sealed and expired row's table is attached with the row's bound | live |
| L9 | every segment-named table has a row | live |
| S1-S7 | full segments sealed, open ones counted to the head, two segments of room, nothing left expired or detaching, nothing past its age or bytes bound, no advisory lock left by an idle `evbk-*` session | settled |

| Case | Test |
|---|---|
| a workload with segments of 50 crosses every state on an age- and a bytes-bound topic; no live finding repeats, the settled check finds nothing, and the check leaves the schema's objects as they were | `invariants_hold_after_every_segment_state` |
| each of L1-L9 broken by hand on a quiesced partition is reported, with every row the change causes | `l1_floor_below_zero` ... `l9_a_stray_segment_table` |

## 5. Long-haul run (`benches/longhaul`, `longhaul/`)

`make longhaul-eb-pg` runs three topics (bounded by age, by bytes, unbounded) under steady load with bursts until Ctrl-C, against the server `longhaul/compose.yaml` starts or any `EB_LONGHAUL_URL`. It checks order, gaps and exactly-once as it reads, runs the live invariants every 30 s, prints a status line, and judges plateau, retention bounds and the settled invariants at the end. `longhaul/observe.sql` gives the operator views over the same tables. See `longhaul/README.md`.

Not yet covered:

- startup with the database down at first and up later (`Unavailable`, then served without a restart);
- a server older than PostgreSQL 14 being refused (no older image is pulled);
- two backend instances on one database contending for a partition: covered only by holding the lock another instance would take;
- a detacher connection killed mid-`DETACH ... CONCURRENTLY` (the pending state comes from a statement timeout instead);
- `period` segment sizing from an observed rate end to end.
