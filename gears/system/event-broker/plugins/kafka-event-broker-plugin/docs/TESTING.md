# Testing - Kafka Event Broker Plugin

Two layers. Layer 2 needs a Docker daemon and runs behind the `integration` feature:

```bash
cargo test -p cf-gears-kafka-event-broker-plugin                                                  # layer 1
cargo test -p cf-gears-kafka-event-broker-plugin --features integration -- --test-threads=1       # layers 1 and 2
cargo bench -p cf-gears-kafka-event-broker-plugin --features integration --bench backend -- --quick
```

The broker is `libs/test-containers`' `kafka()`: one `apache/kafka` KRaft node on the pinned tag (`GEARS_TEST_KAFKA_TAG` overrides it), with topic auto-creation off, retention checked every second, and a one-replica transaction state log and offsets topic. `EB_TEST_KAFKA_BOOTSTRAP` points layer 2 at an existing cluster instead of a container, `EB_BENCH_KAFKA_BOOTSTRAP` the bench. Each layer-2 test starts its own container and works under a topic prefix of its own.

## 1. Layer 1 - unit tests (no broker)

| What | Where |
|---|---|
| Settings: defaults, every setting read, `${VAR}` expansion, every rejection naming the setting and not the value (required, empty, types, prefix rule, replication bounds, `min_insync_replicas` over `replication_factor`, `client` allow-list incl. security, isolation and `socket.timeout.ms` refused, `producer_pool` cap, `request_timeout_ms` over `transaction_timeout_ms`, unknown setting), bootstrap servers redacted in `Debug` (§1) | `src/settings_tests.rs` |
| Names: event, logs and fences topics, transactional ids; a fence key's partition; sequence = offset + 1 and the first offset after a cursor (§2.1, §4) | `src/names_tests.rs` |
| Log and fence records: round trips, exact encodings, other versions and shapes read as absent; key bytes (§2.3) | `src/records_tests.rs` |
| Event body: round trip with the record's position, foreign and unknown-version values `Internal` without quoting them (§2.2) | `src/body_tests.rs` |
| Producer pool: never more keys than its capacity, a new key waits while every key is busy and gets the slot once one is idle; one key shares its slot (§3.2) | `src/fence_tests.rs` |
| Error mapping: transient codes `Unavailable`, admin codes recognised, other codes per operation, codeless client errors `Internal` without librdkafka's text, purge after fencing counted as fenced (§7) | `src/error_tests.rs` |
| Topic configuration: unset bounds `-1`, an age bound sets `segment.ms`, capped at 7 days (§6) | `src/provision_tests.rs` |

## 2. Layer 2 - against a broker

### 2.1 Conformance (`tests/conformance.rs`)

The shared suite (`plugins/event-broker-backend-conformance`, all scenarios, `run_all`) against one container: `conformance`. `build` uses one topic prefix, `build_isolated` a fresh prefix (topics, logs and fences no other instance sees), `build_unreachable` `127.0.0.1:1`, where nothing listens. `retention_deadline` is 8 s.

### 2.2 Plugin-specific (`tests/kafka.rs`)

| DESIGN | Case | Test |
|---|---|---|
| §3.2 | instance B opening a fence key fences A; a late copy of B's fence at the zombie A is `Duplicate` and stores nothing; A's next fence fences B, whose late copy is `Duplicate` in turn | `a_zombie_producer_is_fenced` |
| §3.2, §3.3 | after a stop and a fresh instance, replays of applied fences are `Duplicate`; with `producer_pool: 1`, a key's producer evicted by another key's append and opened again still refuses a replay | `a_replay_after_a_producer_restart_is_a_duplicate` |
| §5.1 | the first `provision` creates the Kafka topic and answers `Pending` without claiming the log (`NotProvisioned` meanwhile); a later one answers `Ready` with the count | `provisioning_is_pending_until_the_topic_exists` |
| §5.1, §6 | the topic is created with `delete`, `LogAppendTime`, the spec's `retention.ms`/`retention.bytes`, `segment.ms` and `min.insync.replicas`; a re-provision under the same log id with other bounds reconfigures it | `a_retention_change_reconfigures_the_topic` |
| §5.1, §5.7 | events, `Latest` and the log record survive a stop and a fresh instance; a stopped instance is `Unavailable`; another log id is `LogConflict`, the same one `Ready`; numbering continues | `a_restart_keeps_events_heads_and_the_log` |
| §4 | append heads, `Latest` on the appending and on a fresh instance are the last event's sequence, not the watermark the commit marker moved; sequences 1, 2, 4 around the marker; the last stable offset is 5 | `heads_are_the_last_event_never_the_watermark` |
| §5.4, §6 | the broker removes every event past a one-second age; `Latest` stays the last event's sequence on the instance that appended and on a fresh one, and `Earliest` equals it | `latest_survives_removal_of_every_event` |
| §2.1 | a topic provisioned under one prefix is `NotProvisioned` under another on the same cluster | `prefixes_keep_backends_apart` |
| §1, §3.2 | instances configured with fewer or more `fence_partitions` than the fences topic holds find a key's fence in the topic's partition: a replay is `Duplicate`, never applied again or `Unavailable` | `a_mismatched_fence_partitions_setting_keeps_exactly_once` |
| §3.2 | an append dropped mid-transaction (a 3 ms timeout over 1000 events) is aborted: another key's append right after it returns and is readable within 5 s, under a 30 s transaction timeout | `a_dropped_append_does_not_hold_back_readers` |
| §5.7 | once `stop` returns, an append it overlapped has ended: the last stable offset does not move afterwards | `stop_waits_for_an_append_in_flight` |
| §5.1 | on a broker that never ran a transaction, eight topics provisioned at once, each first call cancelled after 50 ms: every later answer within 5 s, only `Ready`, `Pending` or `Unavailable`, all `Ready` within 90 s | `a_cold_broker_answers_provision_quickly_and_survives_a_cancel` |
| §3.1, §5.7 | `stop` with its deadline cancelled returns within 200 ms with an append in flight, and the runtime then shuts down within 2 s | `stop_and_shutdown_are_prompt_past_the_deadline` |

### 2.3 Covered by conformance

| DESIGN | Case | Scenario |
|---|---|---|
| §5.1 | log id compare-and-set; the held partition count; `NotProvisioned` everywhere | `provision_*`, `not_provisioned_everywhere` |
| §5.2 | heads per touched partition, rising sequences with gaps, key order across instances, the 1000-event limit, two instances on one partition | `append_*`, `concurrent_appenders_assign_distinct_sequences` |
| §3.2 | retry, late copy, gaps in fence seqs, keys kept apart, fence state across a rebuild | `dedup_*` |
| §5.3 | exclusive and bounded reads; a waiting read woken by a local append and by another instance's | `read_*`, `tail_read_sees_another_instance` |
| §5.4 | `Earliest`, `Latest`, `At` by storage time, `Exact` and `OffsetOutOfRange` | `resolve_positions` |
| §5.6 | the polled watch's first head and named partitions only | `watch_*` |
| §6 | nothing removed within bounds; age by storage time; oldest first and `Latest` kept | `retention_*` |
| §5.7 | `start` with the cluster unreachable, operations `Unavailable`; `stop` idempotent and before `start` | `start_succeeds_with_the_medium_unreachable`, `stop_*` |

## 3. Bench (`benches/backend.rs`)

The shared bench (`plugins/event-broker-backend-bench`) against a container, every instance with the default settings but one replica; `Sizing::connections` becomes `consumer_pool`. A quick smoke:

```bash
cargo bench -p cf-gears-kafka-event-broker-plugin --features integration --bench backend -- \
  --quick --scenarios b1,b2,b3 --cells 'b1/*,b2/batch-16p16c-*,b2/single-1p1c-*,b3/*'
```
