//! Plugin-specific behaviour over a real database file (`docs/TESTING.md`
//! layer 3). Each test has a directory of its own.

#![allow(unknown_lints)]
#![allow(
    de0706_no_direct_sqlx,
    reason = "the tests inspect and tamper with the plugin's file directly"
)]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "a failed expectation is the test failure"
)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Fence, FenceKey, FenceSeq, Flush, Head,
    KeyedEvent, LogClaim, Partition, Replacing, Retention, Role, TopicPartition, TopicSpec, Wait,
    Watch,
};
use event_broker_sdk::models::{Event, PartitionRange};
use event_broker_sdk::{GtsInstanceId, GtsTypeId, Position, Sequence};
use futures_util::StreamExt;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const STORED_AT_LEAST: DateTime<Utc> = DateTime::from_timestamp_nanos(1_767_225_600_000_000_000);

/// An event of fixed size: every field but `subject` is constant, and
/// `subject` is `key-n` with `n` of one digit.
fn event(key: &str, n: u64) -> KeyedEvent {
    KeyedEvent {
        key: key.to_owned(),
        event: Event {
            id: Uuid::from_u128(0x5eed_0000 + u128::from(n)),
            type_id: GtsTypeId::new(toolkit_gts::gts_id!(
                "cf.core.events.event.v1~cf.sqlite_test.x.happened.v1~"
            )),
            tenant_id: Uuid::from_u128(0x7e4a_0001),
            source: "sqlite-test".to_owned(),
            subject: format!("{key}-{n}"),
            subject_type: GtsTypeId::new(toolkit_gts::gts_id!("cf.sqlite_test.x.thing.v1~")),
            occurred_at: STORED_AT_LEAST,
            trace_parent: None,
            data: Some(serde_json::json!({ "key": key, "n": n })),
            partition: None,
            sequence: None,
            sequence_time: None,
            meta: None,
        },
    }
}

fn fence(key: u128, seq: i64) -> Fence {
    Fence {
        key: FenceKey::new(key),
        seq: FenceSeq::new(seq),
    }
}

async fn append(
    backend: &dyn Backend,
    fence: Fence,
    topic: &GtsInstanceId,
    events: &[KeyedEvent],
) -> Result<Appended, BackendError> {
    backend
        .append(&AppendRequest {
            fence,
            topic,
            events,
            flush: Flush::Sync,
        })
        .await
}

fn head(topic: &GtsInstanceId, partition: u32, sequence: i64) -> Head {
    Head {
        at: TopicPartition::new(topic.clone(), Partition::new(partition)),
        sequence: Sequence::assigned(sequence),
    }
}

async fn sequences(backend: &dyn Backend, at: &TopicPartition) -> Vec<i64> {
    backend
        .read(at, Sequence::NONE, 10_000, Wait::No)
        .await
        .expect("read")
        .iter()
        .map(|e| e.sequence.expect("sequence").as_i64())
        .collect()
}

async fn resolved(backend: &dyn Backend, at: &TopicPartition, position: Position) -> i64 {
    backend
        .resolve(at, position)
        .await
        .expect("resolve")
        .as_i64()
}

/// A connection of the test's own to the plugin's file.
async fn inspect(path: &Path) -> SqlitePool {
    SqlitePool::connect_with(SqliteConnectOptions::new().filename(path))
        .await
        .expect("the test opens the file")
}

/// Polls `check` every 50 ms until it holds, for up to 5 s.
async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !check().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "not within 5s: {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

const UNBOUNDED: Retention = Retention {
    max_age: None,
    max_bytes: None,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_file_is_wal_with_incremental_vacuum() {
    let files = common::Files::new();
    let path = files.path("events.db");
    let backend = common::started(common::built(&path).await).await;

    let pool = inspect(&path).await;
    let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .unwrap();
    let vacuum: i64 = sqlx::query_scalar("PRAGMA auto_vacuum")
        .fetch_one(&pool)
        .await
        .unwrap();
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name LIKE 'evbk%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        (journal.as_str(), vacuum, tables),
        (
            "wal",
            2,
            vec![
                "evbk__event".to_owned(),
                "evbk__fence".to_owned(),
                "evbk__log".to_owned(),
                "evbk__partition".to_owned()
            ]
        )
    );
    backend.stop(CancellationToken::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_on_the_same_file_keeps_events_heads_and_fences() {
    let files = common::Files::new();
    let path = files.path("events.db");
    let first = common::started(common::built(&path).await).await;
    let spec = common::provisioned(first.as_ref(), "restart", 1, UNBOUNDED).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let appended = append(
        first.as_ref(),
        fence(7, 1),
        &spec.topic,
        &[event("k", 0), event("k", 1), event("k", 2)],
    )
    .await
    .unwrap();
    assert_eq!(appended, Appended::Heads(vec![head(&spec.topic, 0, 3)]));
    first.stop(CancellationToken::new()).await;
    drop(first);

    let second = common::started(common::built(&path).await).await;
    let stored = second
        .read(&at, Sequence::NONE, 10, Wait::No)
        .await
        .unwrap();
    let replay = append(second.as_ref(), fence(7, 1), &spec.topic, &[event("k", 0)])
        .await
        .unwrap();
    let next = append(second.as_ref(), fence(7, 2), &spec.topic, &[event("k", 3)])
        .await
        .unwrap();

    assert_eq!(
        stored
            .iter()
            .map(|e| (e.sequence.unwrap().as_i64(), e.subject.clone(), e.partition))
            .collect::<Vec<_>>(),
        [
            (1, "k-0".to_owned(), Some(0)),
            (2, "k-1".to_owned(), Some(0)),
            (3, "k-2".to_owned(), Some(0))
        ]
    );
    assert_eq!(replay, Appended::Duplicate);
    assert_eq!(next, Appended::Heads(vec![head(&spec.topic, 0, 4)]));
    assert_eq!(sequences(second.as_ref(), &at).await, [1, 2, 3, 4]);
    second.stop(CancellationToken::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appenders_across_instances_are_gapless() {
    let files = common::Files::new();
    let path = files.path("events.db");
    let mut instances = Vec::new();
    for _ in 0..3 {
        instances.push(common::started(common::built(&path).await).await);
    }
    let spec = common::provisioned(instances[0].as_ref(), "concurrent", 1, UNBOUNDED).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));

    let mut writers = Vec::new();
    for (i, backend) in instances.iter().enumerate() {
        let backend = Arc::clone(backend);
        let topic = spec.topic.clone();
        writers.push(tokio::spawn(async move {
            for call in 0..20_i64 {
                let batch: Vec<_> = (0..5).map(|n| event(&format!("w{i}"), n)).collect();
                let fenced = fence(100 + u128::try_from(i).unwrap(), call + 1);
                append(backend.as_ref(), fenced, &topic, &batch)
                    .await
                    .expect("append");
            }
        }));
    }
    for writer in writers {
        writer.await.unwrap();
    }

    let events = instances[1]
        .read(&at, Sequence::NONE, 1000, Wait::No)
        .await
        .unwrap();
    let mut per_writer = [Vec::new(), Vec::new(), Vec::new()];
    for e in &events {
        let data = e.data.as_ref().unwrap();
        let writer: usize = data["key"].as_str().unwrap()[1..].parse().unwrap();
        per_writer[writer].push(data["n"].as_u64().unwrap());
    }
    assert_eq!(
        events
            .iter()
            .map(|e| e.sequence.unwrap().as_i64())
            .collect::<Vec<_>>(),
        (1..=300).collect::<Vec<_>>()
    );
    let in_order: Vec<u64> = (0..20).flat_map(|_| 0..5).collect();
    assert_eq!(per_writer, [in_order.clone(), in_order.clone(), in_order]);
    assert_eq!(
        resolved(instances[2].as_ref(), &at, Position::Latest).await,
        300
    );
    for backend in instances {
        backend.stop(CancellationToken::new()).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_by_bytes_keeps_the_newest_events_within_the_bound() {
    let files = common::Files::new();
    let path = files.path("events.db");
    let backend = common::started(common::built(&path).await).await;
    let spec = common::provisioned(backend.as_ref(), "bytes", 1, UNBOUNDED).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let batch: Vec<_> = (0..5).map(|n| event("k", n)).collect();
    append(backend.as_ref(), fence(1, 1), &spec.topic, &batch)
        .await
        .unwrap();
    let pool = inspect(&path).await;
    let sizes: Vec<i64> = sqlx::query_scalar("SELECT size FROM evbk__event ORDER BY sequence")
        .fetch_all(&pool)
        .await
        .unwrap();
    let size = sizes[0];
    assert_eq!(sizes, [size; 5], "fixed-size events");

    // Two events and a byte fit: the third from the end is over.
    let bound = Retention {
        max_age: None,
        max_bytes: Some(u64::try_from(size * 2 + 1).unwrap()),
    };
    let current: Uuid = sqlx::query_scalar("SELECT log_id FROM evbk__log")
        .fetch_one(&pool)
        .await
        .unwrap();
    let log = event_broker_sdk::backend::LogId::from_uuid(current);
    backend
        .provision(
            &TopicSpec {
                retention: bound,
                ..spec.clone()
            },
            &LogClaim {
                replacing: Replacing::Log(log),
                new: log,
            },
        )
        .await
        .unwrap();
    eventually("the oldest three removed", || async {
        sequences(backend.as_ref(), &at).await == [4, 5]
    })
    .await;
    // Nothing more goes on later passes.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let (floor, hwm, bytes): (i64, i64, i64) =
        sqlx::query_as("SELECT floor, hwm, bytes FROM evbk__partition")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(sequences(backend.as_ref(), &at).await, [4, 5]);
    assert_eq!((floor, hwm, bytes), (3, 5, size * 2));
    assert_eq!(
        [
            resolved(backend.as_ref(), &at, Position::Earliest).await,
            resolved(backend.as_ref(), &at, Position::Latest).await
        ],
        [3, 5]
    );
    backend.stop(CancellationToken::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_by_age_removes_each_event_once_past_it() {
    let files = common::Files::new();
    let path = files.path("events.db");
    let backend = common::started(common::built(&path).await).await;
    let three_seconds = Retention {
        max_age: Some(Duration::from_secs(3)),
        max_bytes: None,
    };
    let spec = common::provisioned(backend.as_ref(), "age", 1, three_seconds).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));

    append(backend.as_ref(), fence(1, 1), &spec.topic, &[event("k", 0)])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    append(backend.as_ref(), fence(1, 2), &spec.topic, &[event("k", 1)])
        .await
        .unwrap();
    // The first goes once past its age; the second, stored two seconds later,
    // stays two seconds longer.
    eventually("the first removed, the second kept", || async {
        sequences(backend.as_ref(), &at).await == [2]
    })
    .await;

    eventually("the second removed", || async {
        sequences(backend.as_ref(), &at).await.is_empty()
    })
    .await;
    assert_eq!(
        [
            resolved(backend.as_ref(), &at, Position::Earliest).await,
            resolved(backend.as_ref(), &at, Position::Latest).await
        ],
        [2, 2]
    );
    backend.stop(CancellationToken::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delivery_instance_runs_no_sweep() {
    let files = common::Files::new();
    let path = files.path("events.db");
    let delivery =
        common::started(common::built_with(&path, Role::Delivery, serde_json::json!({})).await)
            .await;
    let tight = Retention {
        max_age: None,
        max_bytes: Some(1),
    };
    let spec = common::provisioned(delivery.as_ref(), "delivery", 1, tight).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    append(
        delivery.as_ref(),
        fence(1, 1),
        &spec.topic,
        &[event("k", 0)],
    )
    .await
    .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(sequences(delivery.as_ref(), &at).await, [1]);

    let ingest =
        common::started(common::built_with(&path, Role::Ingest, serde_json::json!({})).await).await;
    eventually("the ingest instance's sweep removes it", || async {
        sequences(delivery.as_ref(), &at).await.is_empty()
    })
    .await;
    ingest.stop(CancellationToken::new()).await;
    delivery.stop(CancellationToken::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replayed_fence_on_an_unprovisioned_topic_is_not_provisioned() {
    let files = common::Files::new();
    let backend = common::started(common::built(&files.path("events.db")).await).await;
    let spec = common::provisioned(backend.as_ref(), "fenced", 1, UNBOUNDED).await;
    append(backend.as_ref(), fence(9, 5), &spec.topic, &[event("k", 0)])
        .await
        .unwrap();

    let elsewhere = append(
        backend.as_ref(),
        fence(9, 5),
        &common::topic("unprovisioned"),
        &[event("k", 0)],
    )
    .await;

    assert!(
        matches!(elsewhere, Err(BackendError::NotProvisioned { .. })),
        "{elsewhere:?}"
    );
    backend.stop(CancellationToken::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partition_counts_out_of_range_are_refused() {
    let files = common::Files::new();
    let backend = common::started(common::built(&files.path("events.db")).await).await;
    for partitions in [0, 10_000] {
        let refused = backend
            .provision(
                &TopicSpec {
                    topic: common::topic("too_many"),
                    partitions,
                    retention: UNBOUNDED,
                },
                &LogClaim {
                    replacing: Replacing::Nothing,
                    new: event_broker_sdk::backend::LogId::new_v4(),
                },
            )
            .await;
        assert_eq!(
            format!("{refused:?}"),
            r#"Err(InvalidConfig { detail: "a topic has between 1 and 9999 partitions on the sqlite backend" })"#
        );
    }
    backend.stop(CancellationToken::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_partition_past_the_count_is_not_found() {
    let files = common::Files::new();
    let backend = common::started(common::built(&files.path("events.db")).await).await;
    let spec = common::provisioned(backend.as_ref(), "two", 2, UNBOUNDED).await;
    let beyond = TopicPartition::new(spec.topic.clone(), Partition::new(2));

    let read = backend.read(&beyond, Sequence::NONE, 1, Wait::No).await;
    let resolve = backend.resolve(&beyond, Position::Latest).await;

    assert!(
        matches!(read, Err(BackendError::PartitionNotFound { .. })),
        "{read:?}"
    );
    assert!(
        matches!(resolve, Err(BackendError::PartitionNotFound { .. })),
        "{resolve:?}"
    );
    backend.stop(CancellationToken::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heads_reach_a_watch_on_another_instance() {
    let files = common::Files::new();
    let path = files.path("events.db");
    let writer = common::started(common::built(&path).await).await;
    let watcher = common::started(common::built(&path).await).await;
    let spec = common::provisioned(writer.as_ref(), "watched", 2, UNBOUNDED).await;
    // Keys landing in partition 0 and 1 (`partition_for`).
    let (zero, one) = keys_for_both_partitions();
    append(
        writer.as_ref(),
        fence(1, 1),
        &spec.topic,
        &[event(&zero, 0)],
    )
    .await
    .unwrap();

    let Watch::Native(watch) = watcher.watch() else {
        panic!("the sqlite backend watches natively");
    };
    let mut heads = watch.watch(&[
        TopicPartition::new(spec.topic.clone(), Partition::new(0)),
        TopicPartition::new(spec.topic.clone(), Partition::new(1)),
    ]);
    let mut seen = Vec::new();
    for _ in 0..2 {
        seen.push(next(&mut heads).await);
    }
    append(
        writer.as_ref(),
        fence(1, 2),
        &spec.topic,
        &[event(&zero, 1), event(&one, 0), event(&one, 1)],
    )
    .await
    .unwrap();
    for _ in 0..2 {
        seen.push(next(&mut heads).await);
    }

    assert_eq!(
        seen,
        [
            head(&spec.topic, 0, 1),
            head(&spec.topic, 1, 0),
            head(&spec.topic, 0, 2),
            head(&spec.topic, 1, 2)
        ]
    );
    writer.stop(CancellationToken::new()).await;
    watcher.stop(CancellationToken::new()).await;
}

async fn next(
    heads: &mut futures_util::stream::BoxStream<'static, Result<Head, BackendError>>,
) -> Head {
    tokio::time::timeout(Duration::from_secs(5), heads.next())
        .await
        .expect("a head within 5s")
        .expect("the watch goes on")
        .expect("a head")
}

fn keys_for_both_partitions() -> (String, String) {
    let key = |p| {
        (0..1000)
            .map(|i| format!("key{i}"))
            .find(|k| event_broker_sdk::backend::support::partition_for(k, 2) == Partition::new(p))
            .unwrap()
    };
    (key(0), key(1))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_reports_the_range_and_the_stored_span() {
    let files = common::Files::new();
    let backend = common::started(common::built(&files.path("events.db")).await).await;
    let spec = common::provisioned(backend.as_ref(), "query", 1, UNBOUNDED).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    let batch: Vec<_> = (0..4).map(|n| event("k", n)).collect();
    append(backend.as_ref(), fence(1, 1), &spec.topic, &batch)
        .await
        .unwrap();
    let stored = backend
        .read(&at, Sequence::NONE, 10, Wait::No)
        .await
        .unwrap();

    let segments = backend
        .query(
            &at,
            PartitionRange {
                start_offset: Some(Sequence::assigned(2)),
                end_offset: None,
                limit: 2,
            },
        )
        .await
        .unwrap();
    let empty = backend
        .query(
            &at,
            PartitionRange {
                start_offset: Some(Sequence::assigned(9)),
                end_offset: None,
                limit: 2,
            },
        )
        .await
        .unwrap();

    assert_eq!(segments.len(), 1);
    let segment = &segments[0];
    assert_eq!(
        (
            segment.topic.clone(),
            segment.partition,
            segment.start_sequence,
            segment.end_sequence,
            segment.start_time,
            segment.end_time,
            segment.segments.clone()
        ),
        (
            spec.topic.to_string(),
            0,
            Sequence::assigned(2),
            Sequence::assigned(3),
            stored[1].sequence_time,
            stored[2].sequence_time,
            vec![serde_json::json!({ "from_sequence": 1, "to_sequence": 4 })]
        )
    );
    assert!(empty.is_empty());
    backend.stop(CancellationToken::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_one_instance_leaves_another_on_the_file_running() {
    let files = common::Files::new();
    let path = files.path("events.db");
    let stopped = common::started(common::built(&path).await).await;
    let running = common::started(common::built(&path).await).await;
    let spec = common::provisioned(running.as_ref(), "stop_one", 1, UNBOUNDED).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));

    stopped.stop(CancellationToken::new()).await;
    let after_stop = stopped.read(&at, Sequence::NONE, 1, Wait::No).await;
    append(running.as_ref(), fence(1, 1), &spec.topic, &[event("k", 0)])
        .await
        .unwrap();

    assert!(
        matches!(after_stop, Err(BackendError::Unavailable { .. })),
        "{after_stop:?}"
    );
    assert_eq!(sequences(running.as_ref(), &at).await, [1]);
    running.stop(CancellationToken::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drifted_schema_keeps_the_backend_unavailable() {
    let files = common::Files::new();
    let path = files.path("events.db");
    let first = common::started(common::built(&path).await).await;
    first.stop(CancellationToken::new()).await;
    drop(first);
    let pool = inspect(&path).await;
    sqlx::query("UPDATE _sqlx_migrations SET checksum = x'00'")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let drifted = common::built(&path).await;
    drifted.start().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let probe = TopicPartition::new(common::topic("probe"), Partition::new(0));
    let answer = drifted.resolve(&probe, Position::Latest).await;

    assert_eq!(
        format!("{answer:?}"),
        r#"Err(Unavailable { reason: "the sqlite backend has not opened its file yet", detail: "" })"#
    );
    drifted.stop(CancellationToken::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sequence_time_never_falls_with_sequence() {
    let files = common::Files::new();
    let path = files.path("events.db");
    let backend = common::started(common::built(&path).await).await;
    let spec = common::provisioned(backend.as_ref(), "clock", 1, UNBOUNDED).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));
    // A head time far ahead, as a clock stepped back since would leave it.
    let pool = inspect(&path).await;
    let ahead = STORED_AT_LEAST.timestamp_micros() * 2;
    sqlx::query("UPDATE evbk__partition SET head_time = ?1")
        .bind(ahead)
        .execute(&pool)
        .await
        .unwrap();

    append(backend.as_ref(), fence(1, 1), &spec.topic, &[event("k", 0)])
        .await
        .unwrap();
    let stored = backend
        .read(&at, Sequence::NONE, 1, Wait::No)
        .await
        .unwrap();

    assert_eq!(
        stored[0].sequence_time,
        DateTime::from_timestamp_micros(ahead)
    );
    backend.stop(CancellationToken::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_append_dropped_before_its_commit_stores_nothing() {
    let files = common::Files::new();
    let path = files.path("events.db");
    let backend = common::started(common::built(&path).await).await;
    let spec = common::provisioned(backend.as_ref(), "abandoned", 1, UNBOUNDED).await;
    let at = TopicPartition::new(spec.topic.clone(), Partition::new(0));

    // The file's write lock, held outside the plugin: its append waits in
    // `BEGIN IMMEDIATE` while its caller gives up.
    let pool = inspect(&path).await;
    let holder = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let abandoned = tokio::time::timeout(
        Duration::from_millis(300),
        append(backend.as_ref(), fence(1, 1), &spec.topic, &[event("k", 0)]),
    )
    .await;
    assert!(abandoned.is_err(), "the append outlived its caller's wait");
    holder.rollback().await.unwrap();
    // Longer than any write would take once the lock is free.
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(sequences(backend.as_ref(), &at).await, Vec::<i64>::new());
    assert_eq!(resolved(backend.as_ref(), &at, Position::Latest).await, 0);
    // Nothing of it stayed, the fence included: the same fence applies.
    assert_eq!(
        append(backend.as_ref(), fence(1, 1), &spec.topic, &[event("k", 0)])
            .await
            .unwrap(),
        Appended::Heads(vec![head(&spec.topic, 0, 1)])
    );
    backend.stop(CancellationToken::new()).await;
}
