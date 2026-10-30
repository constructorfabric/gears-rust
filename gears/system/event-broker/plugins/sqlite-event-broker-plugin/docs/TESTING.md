# Testing - SQLite Event Broker Plugin

Three layers, all in a plain `cargo test`: the medium is a file in a temporary directory, so nothing needs a feature gate, a container or a daemon.

```bash
cargo test -p cf-gears-sqlite-event-broker-plugin                          # layers 1-3
cargo bench -p cf-gears-sqlite-event-broker-plugin --bench backend -- --quick   # the bench, a smoke run
```

## 1. Layer 1 - unit tests (no file)

| What | Where |
|---|---|
| Settings: defaults with only `path`, every setting read, a partial `sweep` keeping the other default, unknown settings refused by name at both levels, bad values refused without quoting them (`path` empty, `:memory:` and `file:` URIs among them), `path` expansion and an unset variable; intervals and timeouts over one day and `sweep.batch` over 100000 refused, the bounds themselves accepted | `src/settings_tests.rs` |
| Error mapping: other codes `AppendFailed` in `append`, `ReadFailed` in every read path, `Internal` in `open`, `provision` and the sweep; transient result codes (primary and extended) `Unavailable` in every operation; lost pool and I/O `Unavailable`; driver failures `Internal`; no message quotes SQLite's or the driver's text (§7) | `src/error_tests.rs` |
| Body encoding: versioned JSON without `meta` or positions, decoded at its position, unknown `v` refused (§2.3) | `src/body_tests.rs` |
| Stored instants: whole microseconds round trip, a sub-microsecond fraction rounds down or up, out of range is `Internal` | `src/clock_tests.rs` |
| The byte cut: the first event that sheds the excess, a batch short of it going whole, nothing stored; a partition within its bounds is left unlocked, one past either bound is swept (§4.8) | `src/sweep_tests.rs` |
| The detached commit: a commit whose caller drops it once issued still lands, releases the write turn and publishes its heads (§4.2) | `src/append_tests.rs` |
| The hub registry: two spellings of one file share a hub, different files do not, a hub lives only while held, a missing directory is an error, a symlink to the file shares its hub, a file created after its hub keeps it (§3.2) | `src/hub_tests.rs` |

## 2. Layer 2 - conformance (`tests/conformance.rs`)

The shared suite (`plugins/event-broker-backend-conformance`, every scenario) once per `synchronous` mode: `conformance` (`full`) and `conformance_with_synchronous_normal` (`normal`). `build` opens one file in the test's directory - every instance shares it and its hub; `build_isolated` a file of its own; `build_unreachable` a path under a regular file, which no privilege makes writable. The sweep runs every 100 ms and `retention_deadline` is 2 s.

The suite allows gaps; this plugin's own gapless guarantee is checked in layer 3.

## 3. Layer 3 - plugin-specific (`tests/sqlite.rs`)

Each test has a directory of its own and inspects the file with a connection of its own where it needs to.

| DESIGN | Case | Test |
|---|---|---|
| §3.1 | the file is in WAL mode with incremental auto-vacuum, and holds the four plugin tables | `the_file_is_wal_with_incremental_vacuum` |
| §3.1 | operations are `Unavailable` until startup opens the file; a path that cannot be opened stays so | every test's `common::started`; conformance `start_succeeds_with_the_medium_unreachable` |
| §3.3 | a recorded migration that no longer matches keeps the backend `Unavailable` | `a_drifted_schema_keeps_the_backend_unavailable` |
| §3.1 | stopping one instance leaves another over the file serving, and the stopped one answers `Unavailable` | `stopping_one_instance_leaves_another_on_the_file_running` |
| §4.1 | partition counts 0 and 10000 are refused with the rule | `partition_counts_out_of_range_are_refused` |
| §4.2 | three instances over one file appending to one partition at once assign exactly `1..=300`, each writer's events in order | `concurrent_appenders_across_instances_are_gapless` |
| §4.2, §4.3 | a restart on the same file keeps the events, the head and the fences: the replayed fence is `Duplicate`, the next one continues at 4 | `a_restart_on_the_same_file_keeps_events_heads_and_fences` |
| §4.2 | an append whose caller gives up while it waits for the write lock stores nothing, its fence included | `an_append_dropped_before_its_commit_stores_nothing` |
| §4.2 | `sequence_time` never falls below the partition's `head_time` when the clock is behind it | `sequence_time_never_falls_with_sequence` |
| §4.2 | a fence replayed on an unprovisioned topic is `NotProvisioned`, not `Duplicate` | `a_replayed_fence_on_an_unprovisioned_topic_is_not_provisioned` |
| §4.4, §4.5 | a partition at or above the topic's count is `PartitionNotFound` for `read` and `resolve` | `a_partition_past_the_count_is_not_found` |
| §4.6 | a watch on one instance yields the current heads of every named partition in order, then the heads an append through another instance left on both | `heads_reach_a_watch_on_another_instance` |
| §4.7 | `query` reports the matched range's sequences and times and the stored span as its segment; a range past the head is empty | `query_reports_the_range_and_the_stored_span` |
| §4.8 | by bytes: with a bound of two events and a byte, exactly the oldest three go; `floor`, `hwm`, `bytes`, `Earliest` and `Latest` follow; nothing more goes on later passes | `retention_by_bytes_keeps_the_newest_events_within_the_bound` |
| §4.8 | by age, per event: the first event goes once past its age while the second, stored two seconds later, stays; then the second goes; `Latest` is kept | `retention_by_age_removes_each_event_once_past_it` |
| §4.8 | a delivery instance runs no sweep; an ingest instance over the same file does | `a_delivery_instance_runs_no_sweep` |
| §7 | `NotProvisioned`, `LogConflict`, `OffsetOutOfRange`, `Unavailable` for an unopenable file | conformance |

## 4. Bench (`benches/backend.rs`)

The shared bench (`plugins/event-broker-backend-bench`) over a file in a temporary directory, or the file `EB_BENCH_SQLITE_PATH` names. A temporary directory on `tmpfs` makes every sync free: to measure `synchronous=FULL`, point `EB_BENCH_SQLITE_PATH` at the disk the file will live on.

Not yet covered:

- startup with the directory missing at first and created later (`Unavailable`, then served without a restart);
- `synchronous=FULL` surviving a power loss (needs a machine to cut);
- a second process on the file (unsupported, §1.1);
- the incremental vacuum shrinking the file.
