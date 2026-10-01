# Testing - Postgres Event Broker Plugin

Three layers. Layers 2 and 3 need a Docker daemon and run behind the `integration` feature:

```bash
cargo test -p cf-gears-postgres-event-broker-plugin                          # layer 1
cargo test -p cf-gears-postgres-event-broker-plugin --features integration   # layers 1-3
make test-eb-pg                                                              # the same, via nextest
```

The PostgreSQL image is `libs/test-containers`' pinned tag (`GEARS_TEST_PG_TAG` overrides it). `EB_TEST_POSTGRES_URL` points layers 2 and 3 at an existing server instead of a container.

## 1. Layer 1 - unit tests (no database)

| What | Where |
|---|---|
| Settings: defaults, every setting read, unknown setting refused, bad values refused without quoting them, `schema` identifier rule, `pgbouncer_transaction_mode: true` refused, `url` expansion and redaction | `src/settings_tests.rs` |
| Object names: parent, segment and index names, the 63-byte limit at 9999 partitions | `src/names_tests.rs` |
| Body encoding: round trip, `meta` dropped, unknown `v` refused | `src/body_tests.rs` |
| Segment span: `events`, `period` from the observed rate, the 1 event/s floor; the "half used" threshold | `src/segment_tests.rs` |
| Partition bound parsing from `pg_get_expr(relpartbound)` | `src/segment_tests.rs` |

## 2. Layer 2 - conformance (`tests/conformance.rs`)

The shared suite (`plugins/event-broker-backend-conformance`, all scenarios) against one container. `build` names the container's database and one schema; `build_isolated` a schema of its own; `build_unreachable` a port nothing listens on.

## 3. Layer 3 - plugin-specific (`tests/postgres.rs`)

Each test starts its own server (or uses `EB_TEST_POSTGRES_URL`) and a schema of its own.

| DESIGN | Case | Test |
|---|---|---|
| §3.1 | the default isolation, server version and schema are checked, and operations are `Unavailable` until startup completes | every test's `common::started`; conformance `build_succeeds_with_the_medium_unreachable` |
| §3.2 | migrations are recorded in the plugin's own schema; a recorded migration that no longer matches keeps the backend `Unavailable` | `a_drifted_schema_fails_startup` |
| §4.1 | `provision` creates one parent and one first segment per partition, and a counter row each | `provision_creates_parents_and_first_segments` |
| §4.1 | more than 9999 partitions is refused | `too_many_partitions_is_refused` |
| §4.2 | one row lock per partition: two instances appending to one partition get distinct, gap-free sequences | conformance `concurrent_appenders_assign_distinct_sequences` |
| §4.3 | a late copy after a later fence stores nothing; a retry is `Duplicate` | conformance `dedup_*` |
| §4.3 | a retry blocked on a hung transaction's fence row fails on `lock_timeout` (5 s) instead of waiting | `a_hung_fence_row_times_out_the_retry` |
| §4.7 | after the `LISTEN` connection is killed, a head committed meanwhile reaches a watch | `listener_reconnect_re_emits_heads` |
| §4.9 | a segment is attached ahead once half of the current one is used; appends run on through it | `segments_are_created_ahead` |
| §4.9 | an expired segment is detached and dropped, the rest of the expired prefix deleted, counters to zero; `Latest` unchanged | `expired_segments_are_dropped` |
| §4.9 | retention by bytes, by age (storage time, not `occurred_at`), and adopting new bounds | conformance `retention_*`, `reprovisioning_adopts_new_bounds` |
| §6 | `NotProvisioned`, `LogConflict`, `OffsetOutOfRange`, `Unavailable` for an unreachable server | conformance |

Not yet covered:

- startup with the database down at first and up later (`Unavailable`, then served without a restart);
- two instances' maintenance workers taking turns on one partition (the advisory lock);
- a server older than PostgreSQL 14 being refused (no older image is pulled).
