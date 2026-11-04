use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, Fence, FenceKey, FenceSeq, Flush, Head,
    KeyedEvent, LogClaim, LogId, Partition, Replacing, Retention, TopicPartition, TopicSpec, Wait,
};
use event_broker_sdk::{Event, GtsInstanceId, GtsTypeId, PartitionRange, Position, Sequence};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::MemoryBackend;
use crate::store::Store;

const OCCURRED: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z

fn topic() -> GtsInstanceId {
    GtsInstanceId::try_new("gts.cf.core.events.topic.v1~acme.orders.x.topic.v1").expect("valid id")
}

fn at(partition: u32) -> TopicPartition {
    TopicPartition::new(topic(), Partition::new(partition))
}

fn time(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .expect("valid instant")
}

/// One event as ingest hands it over: no partition, no sequence. 181 stored
/// bytes: 64 fixed + 3 + 1 + 53 + 53 + 7.
fn event(id: u128, occurred_at: DateTime<Utc>) -> Event {
    Event {
        id: Uuid::from_u128(id),
        type_id: GtsTypeId::new("gts.cf.core.events.event.v1~acme.orders.x.created.v1~"),
        tenant_id: Uuid::from_u128(100),
        source: "svc".to_owned(),
        subject: "s".to_owned(),
        subject_type: GtsTypeId::new("gts.cf.core.events.subject.v1~acme.orders.x.order.v1~"),
        occurred_at,
        trace_parent: None,
        data: Some(serde_json::json!({ "n": 1 })),
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: None,
    }
}

/// `event` as the backend stores it.
fn stored(
    id: u128,
    occurred_at: DateTime<Utc>,
    partition: u32,
    sequence: i64,
    now: DateTime<Utc>,
) -> Event {
    Event {
        partition: Some(partition),
        sequence: Some(Sequence::assigned(sequence)),
        sequence_time: Some(now),
        ..event(id, occurred_at)
    }
}

fn keyed(key: &str, id: u128, occurred_at: DateTime<Utc>) -> KeyedEvent {
    KeyedEvent {
        key: key.to_owned(),
        event: event(id, occurred_at),
    }
}

fn fence(seq: i64) -> Fence {
    Fence {
        key: FenceKey::new(0xabc),
        seq: FenceSeq::new(seq),
    }
}

fn provisioned(store: &Store, retention: Retention) {
    store
        .provision(
            &TopicSpec {
                topic: topic(),
                partitions: 2,
                retention,
            },
            &LogClaim {
                replacing: Replacing::Nothing,
                new: LogId::from_uuid(Uuid::from_u128(7)),
            },
        )
        .expect("provisioned");
}

fn append(store: &Store, seq: i64, events: &[KeyedEvent], now: DateTime<Utc>) -> Appended {
    let topic = topic();
    store
        .append(
            &AppendRequest {
                fence: fence(seq),
                topic: &topic,
                events,
                flush: Flush::Async,
            },
            now,
        )
        .expect("appended")
}

fn debug(events: &[Event]) -> String {
    format!("{events:#?}")
}

#[test]
fn append_assigns_partition_and_sequence_and_answers_heads() {
    let store = Store::default();
    provisioned(&store, Retention::default());
    let now = time(OCCURRED + 5);

    // "a" lands on partition 0 of 2, "b" on partition 1.
    let appended = append(
        &store,
        1,
        &[
            keyed("a", 1, time(OCCURRED)),
            keyed("b", 2, time(OCCURRED)),
            keyed("a", 3, time(OCCURRED)),
        ],
        now,
    );

    assert_eq!(
        appended,
        Appended::Heads(vec![
            Head {
                at: at(0),
                sequence: Sequence::assigned(2)
            },
            Head {
                at: at(1),
                sequence: Sequence::assigned(1)
            },
        ])
    );
    assert_eq!(
        debug(&store.read(&at(0), Sequence::NONE, 10).unwrap()),
        debug(&[
            stored(1, time(OCCURRED), 0, 1, now),
            stored(3, time(OCCURRED), 0, 2, now)
        ])
    );
    assert_eq!(
        debug(&store.read(&at(1), Sequence::NONE, 10).unwrap()),
        debug(&[stored(2, time(OCCURRED), 1, 1, now)])
    );
}

#[test]
fn a_fence_at_or_below_the_applied_one_stores_nothing() {
    let store = Store::default();
    provisioned(&store, Retention::default());
    let now = time(OCCURRED);

    append(&store, 5, &[keyed("a", 1, now)], now);
    assert_eq!(
        append(&store, 5, &[keyed("a", 2, now)], now),
        Appended::Duplicate
    );
    assert_eq!(
        append(&store, 4, &[keyed("a", 3, now)], now),
        Appended::Duplicate
    );
    assert_eq!(
        debug(&store.read(&at(0), Sequence::NONE, 10).unwrap()),
        debug(&[stored(1, now, 0, 1, now)])
    );
}

#[test]
fn resolve_answers_floor_ceiling_and_time() {
    let store = Store::default();
    provisioned(&store, Retention::default());
    // Stored at +100 and +120; the producer's `occurred_at` runs the other
    // way, the last event back-dated past them all, and moves nothing.
    append(
        &store,
        1,
        &[
            keyed("a", 1, time(OCCURRED + 50)),
            keyed("a", 2, time(OCCURRED + 60)),
        ],
        time(OCCURRED + 100),
    );
    append(
        &store,
        2,
        &[keyed("a", 3, time(OCCURRED))],
        time(OCCURRED + 120),
    );
    store.truncate(&at(0), Sequence::assigned(1)).unwrap();

    assert_eq!(
        store.resolve(&at(0), &Position::Latest).unwrap(),
        Sequence::assigned(3)
    );
    assert_eq!(
        store.resolve(&at(0), &Position::Earliest).unwrap(),
        Sequence::assigned(1)
    );
    assert_eq!(
        store
            .resolve(&at(0), &Position::At(time(OCCURRED + 110)))
            .unwrap(),
        Sequence::assigned(2)
    );
    assert_eq!(
        store
            .resolve(&at(0), &Position::At(time(OCCURRED + 55)))
            .unwrap(),
        Sequence::assigned(1)
    );
    assert_eq!(
        store
            .resolve(&at(0), &Position::At(time(OCCURRED + 200)))
            .unwrap(),
        Sequence::assigned(3)
    );
    assert_eq!(
        store
            .resolve(&at(0), &Position::Exact(Sequence::assigned(2)))
            .unwrap(),
        Sequence::assigned(2)
    );
    match store.resolve(&at(0), &Position::Exact(Sequence::NONE)) {
        Err(BackendError::OffsetOutOfRange {
            floor,
            ceiling,
            detail,
            ..
        }) => {
            assert_eq!(
                (floor, ceiling, detail.as_str()),
                (
                    Sequence::assigned(1),
                    Sequence::assigned(3),
                    "valid positions are [1, 3]"
                )
            );
        }
        other => panic!("expected OffsetOutOfRange, got {other:?}"),
    }
}

#[test]
fn truncate_keeps_the_head() {
    let store = Store::default();
    provisioned(&store, Retention::default());
    let now = time(OCCURRED);
    append(&store, 1, &[keyed("a", 1, now), keyed("a", 2, now)], now);

    store.truncate(&at(0), Sequence::assigned(2)).unwrap();

    assert_eq!(
        debug(&store.read(&at(0), Sequence::NONE, 10).unwrap()),
        debug(&[])
    );
    assert_eq!(
        store.resolve(&at(0), &Position::Latest).unwrap(),
        Sequence::assigned(2)
    );
    assert_eq!(
        store.resolve(&at(0), &Position::Earliest).unwrap(),
        Sequence::assigned(2)
    );
}

/// Age is when the backend stored an event (`sequence_time`), never the
/// producer's `occurred_at`: an event back-dated by years but stored a moment
/// ago is kept, and one stored long ago is removed whatever time it claims.
#[test]
fn retention_ages_events_by_storage_time_not_by_occurred_at() {
    let store = Store::default();
    provisioned(
        &store,
        Retention {
            max_age: Some(Duration::from_mins(1)),
            max_bytes: None,
        },
    );
    let year_2000 = time(946_684_800);
    let far_future = time(OCCURRED + 10_000_000);
    append(&store, 1, &[keyed("a", 1, far_future)], time(OCCURRED));
    append(&store, 2, &[keyed("a", 2, year_2000)], time(OCCURRED + 30));
    append(&store, 3, &[keyed("a", 3, year_2000)], time(OCCURRED + 90));

    store.enforce_retention(time(OCCURRED + 100));

    assert_eq!(
        debug(&store.read(&at(0), Sequence::NONE, 10).unwrap()),
        debug(&[stored(3, year_2000, 0, 3, time(OCCURRED + 90))])
    );
}

#[test]
fn retention_removes_by_bytes_oldest_first() {
    let store = Store::default();
    provisioned(
        &store,
        Retention {
            max_age: None,
            max_bytes: Some(362),
        },
    );
    let now = time(OCCURRED);
    append(
        &store,
        1,
        &[keyed("a", 1, now), keyed("a", 2, now), keyed("a", 3, now)],
        now,
    );

    store.enforce_retention(now);

    assert_eq!(
        debug(&store.read(&at(0), Sequence::NONE, 10).unwrap()),
        debug(&[stored(2, now, 0, 2, now), stored(3, now, 0, 3, now)])
    );
}

/// Provisioning again under the same log id hands the store new bounds, and
/// the next pass applies them.
#[test]
fn reprovisioning_adopts_new_bounds() {
    let store = Store::default();
    provisioned(&store, Retention::default());
    let now = time(OCCURRED);
    append(&store, 1, &[keyed("a", 1, now)], now);
    store.enforce_retention(now);
    assert_eq!(
        debug(&store.read(&at(0), Sequence::NONE, 10).unwrap()),
        debug(&[stored(1, now, 0, 1, now)])
    );

    provisioned(
        &store,
        Retention {
            max_age: None,
            max_bytes: Some(1),
        },
    );
    store.enforce_retention(now);

    assert_eq!(
        debug(&store.read(&at(0), Sequence::NONE, 10).unwrap()),
        debug(&[])
    );
}

#[test]
fn a_topic_not_provisioned_is_refused() {
    let store = Store::default();
    let topic = topic();
    let request = AppendRequest {
        fence: fence(1),
        topic: &topic,
        events: &[keyed("a", 1, time(OCCURRED))],
        flush: Flush::Async,
    };
    assert!(matches!(
        store.append(&request, time(OCCURRED)),
        Err(BackendError::NotProvisioned { .. })
    ));
    assert!(matches!(
        store.read(&at(0), Sequence::NONE, 1),
        Err(BackendError::NotProvisioned { .. })
    ));
    assert!(matches!(
        store.resolve(&at(0), &Position::Latest),
        Err(BackendError::NotProvisioned { .. })
    ));
}

#[test]
fn query_describes_the_matched_range_as_one_segment() {
    let store = Store::default();
    provisioned(&store, Retention::default());
    let now = time(OCCURRED);
    append(
        &store,
        1,
        &[keyed("a", 1, now), keyed("a", 2, now), keyed("a", 3, now)],
        now,
    );

    let segments = store
        .query(
            &at(0),
            &PartitionRange {
                start_offset: Some(Sequence::assigned(2)),
                end_offset: None,
                limit: 10,
            },
        )
        .unwrap();

    assert_eq!(
        format!("{segments:?}"),
        format!(
            "{:?}",
            vec![event_broker_sdk::TopicSegment {
                topic: topic().to_string(),
                partition: 0,
                start_sequence: Sequence::assigned(2),
                end_sequence: Sequence::assigned(3),
                start_time: Some(now),
                end_time: Some(now),
                segments: vec![
                    serde_json::json!({ "start_sequence": 2, "end_sequence": 3, "event_count": 2 })
                ],
            }]
        )
    );
}

#[tokio::test]
async fn backend_declares_fenced_and_a_limit() {
    let backend = MemoryBackend::new();
    assert_eq!(backend.dedup(), Dedup::Fenced);
    assert_eq!(backend.max_events_per_append(), 1000);
}

#[tokio::test]
async fn an_append_over_the_limit_is_refused() {
    let backend = MemoryBackend::new();
    let topic = topic();
    let events: Vec<KeyedEvent> = (0..1001).map(|id| keyed("a", id, time(OCCURRED))).collect();
    let refused = backend
        .append(&AppendRequest {
            fence: fence(1),
            topic: &topic,
            events: &events,
            flush: Flush::Async,
        })
        .await;
    match refused {
        Err(BackendError::AppendFailed { reason, detail }) => {
            assert_eq!(
                (reason.as_str(), detail.as_str()),
                ("an append carries at most 1000 events", "")
            );
        }
        other => panic!("expected AppendFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn a_waiting_read_returns_on_append() {
    let backend = MemoryBackend::new();
    backend
        .provision(
            &TopicSpec {
                topic: topic(),
                partitions: 2,
                retention: Retention::default(),
            },
            &LogClaim {
                replacing: Replacing::Nothing,
                new: LogId::from_uuid(Uuid::from_u128(7)),
            },
        )
        .await
        .unwrap();
    let reader = backend.clone();
    let waiting = tokio::spawn(async move {
        reader
            .read(
                &at(0),
                Sequence::NONE,
                10,
                Wait::For(Duration::from_secs(5)),
            )
            .await
            .unwrap()
            .len()
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let topic = topic();
    backend
        .append(&AppendRequest {
            fence: fence(1),
            topic: &topic,
            events: &[keyed("a", 1, time(OCCURRED))],
            flush: Flush::Async,
        })
        .await
        .unwrap();
    assert_eq!(waiting.await.unwrap(), 1);
}

/// Provisions a topic whose `max_bytes` no event fits under, and appends two
/// events to partition 0.
async fn fill_past_the_bound(backend: &MemoryBackend) {
    backend
        .provision(
            &TopicSpec {
                topic: topic(),
                partitions: 2,
                retention: Retention {
                    max_age: None,
                    max_bytes: Some(1),
                },
            },
            &LogClaim {
                replacing: Replacing::Nothing,
                new: LogId::from_uuid(Uuid::from_u128(7)),
            },
        )
        .await
        .unwrap();
    let topic = topic();
    backend
        .append(&AppendRequest {
            fence: fence(1),
            topic: &topic,
            events: &[keyed("a", 1, time(OCCURRED)), keyed("a", 2, time(OCCURRED))],
            flush: Flush::Async,
        })
        .await
        .unwrap();
}

/// Waits up to 2 s for partition 0 to be empty.
async fn emptied(backend: &MemoryBackend) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !backend
        .read(&at(0), Sequence::NONE, 10, Wait::No)
        .await
        .unwrap()
        .is_empty()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "still stored after 2s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// With nobody driving it, the started worker removes events past their
/// bound, down to an empty partition; `Latest` stays where it was.
#[tokio::test]
async fn the_retention_worker_runs_on_its_own() {
    let backend = MemoryBackend::over(Arc::default(), Duration::from_millis(20));
    backend.start().await.unwrap();
    fill_past_the_bound(&backend).await;

    emptied(&backend).await;

    assert_eq!(
        backend.resolve(&at(0), Position::Latest).await.unwrap(),
        Sequence::assigned(2)
    );
    backend.stop(CancellationToken::new()).await;
}

/// Stopping one instance stops its worker only: another over the same store
/// still removes.
#[tokio::test]
async fn stopping_an_instance_leaves_its_sibling_running() {
    let store: Arc<Store> = Arc::default();
    let stopped = MemoryBackend::over(Arc::clone(&store), Duration::from_millis(20));
    let sibling = MemoryBackend::over(store, Duration::from_millis(20));
    stopped.start().await.unwrap();
    sibling.start().await.unwrap();
    stopped.stop(CancellationToken::new()).await;

    fill_past_the_bound(&stopped).await;

    emptied(&stopped).await;
    sibling.stop(CancellationToken::new()).await;
}
